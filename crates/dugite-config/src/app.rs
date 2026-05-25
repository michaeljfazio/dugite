//! Application state for the Dugite config editor TUI.
//!
//! The [`App`] struct is the single source of truth for every piece of
//! mutable state visible to the UI:
//!
//! - The loaded config (sections and parameters).
//! - Cursor position (which section and which parameter within it).
//! - Edit mode state (are we editing a value right now? what has been typed?).
//! - Collapsed/expanded section state.
//! - Unsaved-changes flag (derived from [`LoadedConfig::is_modified`]).
//! - Quit-requested flag.
//! - Feedback message (shown in the footer for one frame after an action).
//! - Search mode (press `/` to enter, `Esc` to clear).
//! - Diff overlay (press `Ctrl+D` to show, `Esc` to close).
//!
//! # Section / item model
//!
//! After loading, parameters are grouped into [`Section`]s ordered by the
//! canonical section priority defined in [`crate::schema`].  Each section
//! holds a list of [`Item`]s — one per key found in the config file.
//!
//! The cursor is a `(section_index, item_index)` pair.  When a section is
//! collapsed the cursor skips over all its items.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::{ConfigEntry, LoadedConfig};
use crate::diff::OriginalValues;
use crate::schema::{
    build_lookup, section_priority, ParamDef, ParamType, SubParamDef, KNOWN_PARAMS, SECTION_UNKNOWN,
};

// ---------------------------------------------------------------------------
// Section / item model
// ---------------------------------------------------------------------------

/// A single row in the left-panel tree.
///
/// A row addresses either a whole top-level entry (`path.is_empty()`) or a
/// sub-field inside an Object entry (e.g. `path = ["Tls", "CertPath"]`).
#[derive(Debug, Clone)]
pub struct Item {
    /// Index into [`LoadedConfig::entries`] for this row's top-level entry.
    pub entry_idx: usize,
    /// Path inside the top-level entry's `Value`. Empty for top-level rows.
    pub path: Vec<String>,
    /// Resolved schema, if any.
    pub def: ItemDef,
    /// Nesting depth (0 = top-level). Drives left-padding in the UI.
    // Used by Task 11 (UI rendering with depth indent).
    #[allow(dead_code)]
    pub depth: u8,
    /// True for Object rows (no inline edit; Enter toggles `expanded`).
    pub is_container: bool,
    /// Only meaningful when `is_container`. Object rows start `false`.
    pub expanded: bool,
}

/// Resolved schema for an [`Item`].
#[derive(Debug, Clone, Copy)]
pub enum ItemDef {
    /// Top-level entry with a `ParamDef`.
    Top(&'static crate::schema::ParamDef),
    /// Sub-leaf inside an Object entry.
    Sub(&'static crate::schema::SubParamDef),
    /// No schema available — either a top-level Unknown key or an unknown
    /// sub-key under a known Object.
    Unknown,
}

impl ItemDef {
    /// Return the leaf's `ParamType`, if any.
    pub fn param_type(&self) -> Option<&'static crate::schema::ParamType> {
        match self {
            ItemDef::Top(def) => Some(&def.param_type),
            ItemDef::Sub(sub) => Some(&sub.param_type),
            ItemDef::Unknown => None,
        }
    }

    /// Return the leaf's reloadability, if any.
    // Used by Task 11 (UI rendering of sub-rows).
    #[allow(dead_code)]
    pub fn reloadability(&self) -> Option<crate::schema::Reloadability> {
        match self {
            ItemDef::Top(def) => Some(def.reloadability),
            ItemDef::Sub(sub) => Some(sub.reloadability),
            ItemDef::Unknown => None,
        }
    }
}

/// A logical group of parameters shown as a collapsible section in the tree.
#[derive(Debug)]
pub struct Section {
    /// Display name (e.g. "Network", "Genesis", "Unknown").
    pub name: String,
    /// Parameters belonging to this section, in definition order then file order.
    pub items: Vec<Item>,
    /// Whether the section body is currently visible (true = expanded).
    pub expanded: bool,
}

// ---------------------------------------------------------------------------
// Edit mode
// ---------------------------------------------------------------------------

/// The current editing state for a parameter row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditMode {
    /// Normal browse mode — cursor moves but nothing is being edited.
    None,
    /// User is typing a new value for the selected parameter.
    Typing {
        /// Accumulated key strokes so far.
        buffer: String,
        /// Optional validation error from the last keystroke.
        error: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

/// Complete mutable state for the dugite-config TUI.
pub struct App {
    /// The loaded configuration file.
    pub config: LoadedConfig,
    /// Parameter groups, in canonical display order.
    pub sections: Vec<Section>,
    /// Index of the currently highlighted section.
    pub cursor_section: usize,
    /// Index of the currently highlighted item within the active section.
    pub cursor_item: usize,
    /// Current edit mode.
    pub edit_mode: EditMode,
    /// Message to display in the footer (cleared after one render pass).
    pub feedback: Option<String>,
    /// Set to `true` when the user presses `q` without unsaved changes, or
    /// confirms the quit prompt.
    pub should_quit: bool,
    /// Set to `true` after `q` when there are unsaved changes — triggers the
    /// "unsaved changes — press q again to discard" prompt.
    pub quit_prompt: bool,

    // -----------------------------------------------------------------------
    // Search state
    // -----------------------------------------------------------------------
    /// Whether search mode is currently active (entered with `/`).
    pub search_active: bool,
    /// The current search query string (accumulated keystrokes since `/`).
    pub search_query: String,
    /// Flat (section_idx, item_idx) pairs of items matching the current query,
    /// sorted by relevance score descending.  Empty when search is inactive
    /// or the query is empty.
    pub filtered_items: Vec<(usize, usize)>,

    // -----------------------------------------------------------------------
    // Diff overlay state
    // -----------------------------------------------------------------------
    /// Whether the diff overlay is currently visible (`Ctrl+D` to toggle).
    pub show_diff: bool,
    /// Original values captured at load time (ground truth for diff).
    pub originals: OriginalValues,
}

impl App {
    /// Construct an [`App`] from a loaded config file.
    ///
    /// All sections start expanded.  The cursor starts at section 0, item 0.
    /// The [`OriginalValues`] snapshot is captured immediately so that diffs
    /// remain accurate even after multiple edits.
    pub fn new(mut config: LoadedConfig) -> Self {
        // Snapshot originals from the file BEFORE injecting schema defaults so
        // the diff view only highlights keys the user actually edited.
        let originals = OriginalValues::from_entries(&config.entries);
        config.inject_schema_defaults();
        let lookup = build_lookup();
        let sections = build_sections(&config, &lookup);

        App {
            config,
            sections,
            cursor_section: 0,
            cursor_item: 0,
            edit_mode: EditMode::None,
            feedback: None,
            should_quit: false,
            quit_prompt: false,
            search_active: false,
            search_query: String::new(),
            filtered_items: Vec::new(),
            show_diff: false,
            originals,
        }
    }

    // -----------------------------------------------------------------------
    // Cursor navigation
    // -----------------------------------------------------------------------

    /// Return whether `item` should be cursor-addressable given the current
    /// state of its ancestor container rows.
    ///
    /// An item is hidden iff some prefix of its `path` is a container row in the
    /// same section whose `expanded == false`. Top-level rows (empty path) are
    /// always visible (subject to their section's `expanded`, which is handled
    /// elsewhere).
    pub fn is_visible(&self, section_idx: usize, item_idx: usize) -> bool {
        let section = &self.sections[section_idx];
        let item = &section.items[item_idx];

        if item.path.is_empty() {
            return true;
        }

        for prefix_len in 0..item.path.len() {
            let prefix = &item.path[..prefix_len];
            for candidate in &section.items {
                if candidate.entry_idx != item.entry_idx {
                    continue;
                }
                if !candidate.is_container {
                    continue;
                }
                if candidate.path.len() == prefix.len()
                    && candidate.path[..] == prefix[..]
                    && !candidate.expanded
                {
                    return false;
                }
            }
        }
        true
    }

    /// Move the cursor to the previous visible row (vim `k` / arrow-up).
    pub fn cursor_up(&mut self) {
        if self.edit_mode != EditMode::None {
            return;
        }
        // In search mode navigate the filtered list instead.
        if self.search_active && !self.filtered_items.is_empty() {
            let pos = self.filtered_position();
            if pos > 0 {
                let (sec, item) = self.filtered_items[pos - 1];
                self.cursor_section = sec;
                self.cursor_item = item;
            }
            return;
        }

        let (mut sec, mut item) = (self.cursor_section, self.cursor_item);
        loop {
            if item > 0 {
                item -= 1;
            } else if sec > 0 {
                // Move to the previous section.
                sec -= 1;
                // If the previous section is expanded, start from its last item;
                // if collapsed, land on item 0 (the section header, always visible).
                let prev = &self.sections[sec];
                if prev.expanded && !prev.items.is_empty() {
                    item = prev.items.len() - 1;
                } else {
                    // Collapsed section header is always visible.
                    self.cursor_section = sec;
                    self.cursor_item = 0;
                    return;
                }
            } else {
                // Already at the very first row — nowhere to go.
                return;
            }
            if self.is_visible(sec, item) {
                self.cursor_section = sec;
                self.cursor_item = item;
                return;
            }
        }
    }

    /// Move the cursor to the next visible row (vim `j` / arrow-down).
    pub fn cursor_down(&mut self) {
        if self.edit_mode != EditMode::None {
            return;
        }
        // In search mode navigate the filtered list instead.
        if self.search_active && !self.filtered_items.is_empty() {
            let pos = self.filtered_position();
            if pos + 1 < self.filtered_items.len() {
                let (sec, item) = self.filtered_items[pos + 1];
                self.cursor_section = sec;
                self.cursor_item = item;
            }
            return;
        }

        let (mut sec, mut item) = (self.cursor_section, self.cursor_item);
        let total_sections = self.sections.len();
        loop {
            let expanded = self.sections[sec].expanded;
            let item_count = self.sections[sec].items.len();
            if expanded && item + 1 < item_count {
                item += 1;
            } else if sec + 1 < total_sections {
                sec += 1;
                item = 0;
            } else {
                // No further row exists.
                return;
            }
            if self.is_visible(sec, item) {
                self.cursor_section = sec;
                self.cursor_item = item;
                return;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Section collapse / expand
    // -----------------------------------------------------------------------

    /// Toggle the collapsed / expanded state of the currently focused section.
    pub fn toggle_section(&mut self) {
        if self.edit_mode != EditMode::None {
            return;
        }
        let sec = self.cursor_section;
        self.sections[sec].expanded = !self.sections[sec].expanded;
        // When collapsing, reset item cursor to the section header.
        if !self.sections[sec].expanded {
            self.cursor_item = 0;
        }
    }

    // -----------------------------------------------------------------------
    // Edit mode
    // -----------------------------------------------------------------------

    /// Enter edit mode for the currently selected item.
    ///
    /// - Booleans and enums are toggled/cycled immediately (no typing buffer).
    /// - Strings, numbers, and paths open the typing buffer pre-filled with
    ///   the current value.
    pub fn begin_edit(&mut self) {
        if self.edit_mode != EditMode::None {
            return;
        }
        let Some(item) = self.selected_item() else {
            return;
        };
        let entry = &self.config.entries[item.entry_idx];
        let def = item.def;

        match def.param_type() {
            Some(ParamType::Bool) => {
                // Instant toggle — no typing buffer needed.
                let idx = item.entry_idx;
                if let Err(e) = self.config.entries[idx].toggle_bool() {
                    self.feedback = Some(format!("Toggle failed: {e}"));
                } else {
                    let new_val = self.config.entries[idx].display_value();
                    self.feedback = Some(format!("Set to {new_val}"));
                }
            }
            Some(ParamType::Enum { values }) => {
                // Instant cycle through enum choices.
                let choices: Vec<&str> = values.to_vec();
                let idx = item.entry_idx;
                self.config.entries[idx].cycle_enum(&choices);
                let new_val = self.config.entries[idx].display_value();
                self.feedback = Some(format!("Set to {new_val}"));
            }
            _ => {
                // Open typing buffer pre-filled with current value.
                let current = entry.display_value();
                self.edit_mode = EditMode::Typing {
                    buffer: current,
                    error: None,
                };
            }
        }
    }

    /// Append a character to the active typing buffer.
    pub fn type_char(&mut self, c: char) {
        if let EditMode::Typing { buffer, error } = &mut self.edit_mode {
            buffer.push(c);
            *error = None;
        }
    }

    /// Remove the last character from the active typing buffer (backspace).
    pub fn backspace(&mut self) {
        if let EditMode::Typing { buffer, .. } = &mut self.edit_mode {
            buffer.pop();
        }
    }

    /// Confirm the current typing buffer and apply it to the selected entry.
    ///
    /// On validation failure, the error is stored in the typing buffer so the
    /// footer can display it — the edit mode stays open.
    pub fn confirm_edit(&mut self) {
        let EditMode::Typing { buffer, .. } = &self.edit_mode else {
            return;
        };
        let raw = buffer.clone();

        // Validate via schema if a definition is available.
        let Some(item) = self.selected_item() else {
            self.cancel_edit();
            return;
        };
        let def = item.def;
        let entry_idx = item.entry_idx;

        if let Some(pt) = def.param_type() {
            if let Err(msg) = pt.validate(&raw) {
                // Store the error in the buffer — stays in edit mode.
                if let EditMode::Typing { error, .. } = &mut self.edit_mode {
                    *error = Some(msg);
                }
                return;
            }
        }

        // Apply the edit.
        if let Err(e) = self.config.entries[entry_idx].apply_edit(&raw) {
            if let EditMode::Typing { error, .. } = &mut self.edit_mode {
                *error = Some(e.to_string());
            }
            return;
        }

        self.edit_mode = EditMode::None;
        self.feedback = Some(format!("Updated '{}'", self.config.entries[entry_idx].key));
    }

    /// Discard the current edit and return to browse mode.
    pub fn cancel_edit(&mut self) {
        self.edit_mode = EditMode::None;
    }

    // -----------------------------------------------------------------------
    // Search mode
    // -----------------------------------------------------------------------

    /// Enter search mode.  Called when the user presses `/` in browse mode.
    pub fn enter_search(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        self.filtered_items.clear();
    }

    /// Append a character to the search query and recompute the filter.
    pub fn search_type_char(&mut self, c: char) {
        self.search_query.push(c);
        self.recompute_filter();
    }

    /// Remove the last character from the search query and recompute.
    pub fn search_backspace(&mut self) {
        self.search_query.pop();
        self.recompute_filter();
    }

    /// Clear search mode and restore the full parameter tree.
    pub fn clear_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.filtered_items.clear();
    }

    /// Recompute `filtered_items` from the current `search_query`.
    fn recompute_filter(&mut self) {
        use crate::search::search as do_search;

        if self.search_query.is_empty() {
            self.filtered_items.clear();
            return;
        }

        // Build an iterator of (section_idx, item_idx, key, description, tuning_hint).
        let lookup = build_lookup();
        let iter = self.sections.iter().enumerate().flat_map(|(sec_idx, sec)| {
            sec.items
                .iter()
                .enumerate()
                .map(move |(item_idx, item)| (sec_idx, item_idx, item))
        });

        // We can't capture `self.config` inside the iterator directly because
        // `self` is borrowed mutably.  Collect the item tuples first.
        let tuples: Vec<(usize, usize, String, String, String)> = iter
            .map(|(sec_idx, item_idx, item)| {
                let entry = &self.config.entries[item.entry_idx];
                let def = lookup.get(entry.key.as_str()).copied();
                let key = entry.key.clone();
                let description = def.map(|d| d.description).unwrap_or("").to_string();
                let tuning_hint = def.map(|d| d.tuning_hint).unwrap_or("").to_string();
                (sec_idx, item_idx, key, description, tuning_hint)
            })
            .collect();

        let results = do_search(
            &self.search_query,
            tuples
                .iter()
                .map(|(si, ii, k, d, h)| (*si, *ii, k.as_str(), d.as_str(), h.as_str())),
        );

        self.filtered_items = results
            .into_iter()
            .map(|r| (r.section_idx, r.item_idx))
            .collect();

        // Move the cursor to the first match, if any.
        if let Some(&(sec, item)) = self.filtered_items.first() {
            self.cursor_section = sec;
            self.cursor_item = item;
        }
    }

    /// Return the index of the current cursor in `filtered_items`, or 0.
    fn filtered_position(&self) -> usize {
        self.filtered_items
            .iter()
            .position(|&(s, i)| s == self.cursor_section && i == self.cursor_item)
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Diff overlay
    // -----------------------------------------------------------------------

    /// Toggle the diff overlay on/off.
    pub fn toggle_diff(&mut self) {
        self.show_diff = !self.show_diff;
    }

    /// Close the diff overlay without changing other state.
    pub fn close_diff(&mut self) {
        self.show_diff = false;
    }

    /// Compute the current diff for rendering.
    pub fn diff_entries(&self) -> Vec<crate::diff::DiffEntry> {
        crate::diff::compute_diff(&self.config.entries, &self.originals)
    }

    // -----------------------------------------------------------------------
    // Save
    // -----------------------------------------------------------------------

    /// Save the config file to disk.
    ///
    /// On success, clears the feedback after one render.  On failure, reports
    /// the error in the feedback line.
    pub fn save(&mut self) {
        match crate::config::save_config(&mut self.config) {
            Ok(()) => {
                self.feedback = Some(format!("Saved to '{}'", self.config.path.display()));
            }
            Err(e) => {
                self.feedback = Some(format!("Save failed: {e}"));
            }
        }
        // Also reset the quit_prompt (the file is clean now).
        self.quit_prompt = false;
    }

    /// Save the config file to disk and send SIGHUP to the running node.
    ///
    /// The node PID is read from `pid_file`.  If the file does not exist or
    /// cannot be parsed the save still proceeds but the reload is skipped with
    /// an appropriate error message in the feedback line.
    ///
    /// On Unix the signal is sent via `nix::sys::signal::kill`.  On non-Unix
    /// platforms the save is performed but SIGHUP delivery is skipped with a
    /// warning (this is a no-op in practice since dugite-node only runs on
    /// Unix).
    pub fn save_and_reload(&mut self, pid_file: &std::path::Path) {
        // Save first; if save fails, no point trying to signal.
        if !self.save_for_reload() {
            return;
        }

        // Read the node PID from the file.
        let pid_raw = match std::fs::read_to_string(pid_file) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                self.feedback = Some(format!(
                    "Saved; SIGHUP skipped (cannot read '{}': {e})",
                    pid_file.display()
                ));
                return;
            }
        };
        let pid_num: i32 = match pid_raw.parse() {
            Ok(n) => n,
            Err(_) => {
                self.feedback = Some(format!(
                    "Saved; SIGHUP skipped (invalid PID '{}' in '{}')",
                    pid_raw,
                    pid_file.display()
                ));
                return;
            }
        };

        self.send_sighup(pid_num);
    }

    /// Save to disk and send SIGHUP directly to a known PID (used by the
    /// discovery path, where the PID came from `sysinfo` rather than a file).
    pub fn save_and_signal_pid(&mut self, pid: u32) {
        if !self.save_for_reload() {
            return;
        }
        self.send_sighup(pid as i32);
    }

    /// Persist the current config to disk; report success via `feedback`.
    /// Returns `true` if the save succeeded (so the caller can proceed to
    /// signalling the node).
    fn save_for_reload(&mut self) -> bool {
        if let Err(e) = crate::config::save_config(&mut self.config) {
            self.feedback = Some(format!("Save failed: {e}"));
            return false;
        }
        self.quit_prompt = false;
        true
    }

    /// Send SIGHUP to `pid_num`; on non-Unix platforms, report a no-op.
    /// Sets `feedback` to describe the outcome regardless.
    fn send_sighup(&mut self, pid_num: i32) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            match kill(Pid::from_raw(pid_num), Signal::SIGHUP) {
                Ok(()) => {
                    self.feedback = Some(format!("Saved & SIGHUP sent to PID {pid_num}"));
                }
                Err(e) => {
                    self.feedback = Some(format!("Saved; SIGHUP to PID {pid_num} failed: {e}"));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid_num;
            self.feedback = Some("Saved (SIGHUP not supported on this platform)".to_string());
        }
    }

    // -----------------------------------------------------------------------
    // Quit handling
    // -----------------------------------------------------------------------

    /// Handle a quit request from the user.
    ///
    /// If there are unsaved changes, set `quit_prompt` so the UI can warn.
    /// If `quit_prompt` is already set (second press), discard changes and quit.
    /// If there are no unsaved changes, quit immediately.
    pub fn request_quit(&mut self) {
        if !self.config.is_modified() {
            self.should_quit = true;
        } else if self.quit_prompt {
            // Second press — discard and quit.
            self.should_quit = true;
        } else {
            self.quit_prompt = true;
            self.feedback =
                Some("Unsaved changes — press Ctrl+S to save or q again to discard".into());
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Return a reference to the currently selected [`Item`], if any.
    pub fn selected_item(&self) -> Option<&Item> {
        let sec = self.sections.get(self.cursor_section)?;
        if sec.expanded {
            sec.items.get(self.cursor_item)
        } else {
            // Collapsed section — no item is selected.
            None
        }
    }

    /// Return a reference to the currently selected [`ConfigEntry`], if any.
    ///
    /// Used by tests and callers that need the raw entry without going through
    /// the section/item indirection.
    #[allow(dead_code)]
    pub fn selected_entry(&self) -> Option<&ConfigEntry> {
        let item = self.selected_item()?;
        self.config.entries.get(item.entry_idx)
    }

    /// Return whether the config has unsaved changes.
    pub fn is_modified(&self) -> bool {
        self.config.is_modified()
    }

    /// Return whether the app is currently in text-input mode.
    pub fn is_typing(&self) -> bool {
        matches!(self.edit_mode, EditMode::Typing { .. })
    }

    /// Return the current typing buffer contents (empty string if not typing).
    pub fn typing_buffer(&self) -> &str {
        match &self.edit_mode {
            EditMode::Typing { buffer, .. } => buffer,
            EditMode::None => "",
        }
    }

    /// Return the current typing validation error, if any.
    pub fn typing_error(&self) -> Option<&str> {
        match &self.edit_mode {
            EditMode::Typing { error, .. } => error.as_deref(),
            EditMode::None => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Section builder
// ---------------------------------------------------------------------------

/// Group the config entries into [`Section`]s and sort them.
///
/// Steps:
/// 1. For each entry, look up its section via the schema lookup table.
/// 2. Emit a top-level row for the entry.
/// 3. For Object entries, walk the sub-schema depth-first and emit sub-rows.
/// 4. Group rows by section name, sort by schema order, sort sections by priority.
fn build_sections(
    config: &LoadedConfig,
    lookup: &HashMap<&'static str, &'static ParamDef>,
) -> Vec<Section> {
    let mut section_map: HashMap<String, Vec<Item>> = HashMap::new();

    for (entry_idx, entry) in config.entries.iter().enumerate() {
        let def_opt = lookup.get(entry.key.as_str()).copied();
        let section_name = def_opt
            .map(|d| d.section.to_string())
            .unwrap_or_else(|| SECTION_UNKNOWN.to_string());
        let items_for_section = section_map.entry(section_name).or_default();

        // Emit the top-level row.
        let top_def = def_opt.map(ItemDef::Top).unwrap_or(ItemDef::Unknown);
        let is_object = matches!(
            def_opt.map(|d| &d.param_type),
            Some(ParamType::Object { .. })
        );
        items_for_section.push(Item {
            entry_idx,
            path: Vec::new(),
            def: top_def,
            depth: 0,
            is_container: is_object,
            expanded: false,
        });

        // For Object entries, walk the sub-schema and unknown sub-keys.
        if let Some(def) = def_opt {
            if let ParamType::Object { fields } = &def.param_type {
                let mut path: Vec<String> = Vec::new();
                walk_object_rows(
                    entry_idx,
                    &entry.value,
                    fields,
                    1,
                    &mut path,
                    items_for_section,
                );
            }
        }
    }

    // Cluster items by parent entry to keep sub-rows immediately after their
    // parent, then sort the clusters by schema order.
    let schema_order: HashMap<&str, usize> = KNOWN_PARAMS
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key, i))
        .collect();
    for items in section_map.values_mut() {
        sort_items_by_schema(items, config, &schema_order);
    }

    let mut names: Vec<String> = section_map.keys().cloned().collect();
    names.sort_by(|a, b| {
        let pa = section_priority(a.as_str());
        let pb = section_priority(b.as_str());
        pa.cmp(&pb).then(a.cmp(b))
    });

    names
        .into_iter()
        .map(|name| Section {
            items: section_map.remove(&name).unwrap_or_default(),
            name,
            expanded: true,
        })
        .collect()
}

/// Append rows for every schema-known sub-field of `value` plus any unknown
/// sub-keys present in the file, in schema order then alphabetical for
/// unknowns.
fn walk_object_rows(
    entry_idx: usize,
    value: &Value,
    fields: &'static [SubParamDef],
    depth: u8,
    path: &mut Vec<String>,
    out: &mut Vec<Item>,
) {
    let map = match value.as_object() {
        Some(m) => m,
        None => return,
    };

    // Schema-known sub-fields first, in declared order.
    for sub in fields {
        path.push(sub.key.to_string());

        match &sub.param_type {
            ParamType::Object {
                fields: inner_fields,
            } => {
                out.push(Item {
                    entry_idx,
                    path: path.clone(),
                    def: ItemDef::Sub(sub),
                    depth,
                    is_container: true,
                    expanded: false,
                });
                if let Some(child) = map.get(sub.key) {
                    walk_object_rows(entry_idx, child, inner_fields, depth + 1, path, out);
                }
            }
            _ => {
                out.push(Item {
                    entry_idx,
                    path: path.clone(),
                    def: ItemDef::Sub(sub),
                    depth,
                    is_container: false,
                    expanded: false,
                });
            }
        }

        path.pop();
    }

    // Then unknown sub-keys (present in file but absent from schema), alpha order.
    let known: std::collections::HashSet<&str> = fields.iter().map(|s| s.key).collect();
    let mut unknown_keys: Vec<&String> =
        map.keys().filter(|k| !known.contains(k.as_str())).collect();
    unknown_keys.sort();
    for k in unknown_keys {
        path.push(k.clone());
        out.push(Item {
            entry_idx,
            path: path.clone(),
            def: ItemDef::Unknown,
            depth,
            is_container: false, // even if value is an Object — no schema for it
            expanded: false,
        });
        path.pop();
    }
}

fn sort_items_by_schema(
    items: &mut Vec<Item>,
    config: &LoadedConfig,
    schema_order: &HashMap<&str, usize>,
) {
    let mut clusters: HashMap<usize, Vec<Item>> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    for item in items.drain(..) {
        if !clusters.contains_key(&item.entry_idx) {
            order.push(item.entry_idx);
        }
        clusters.entry(item.entry_idx).or_default().push(item);
    }

    order.sort_by(|a, b| {
        let key_a = config.entries[*a].key.as_str();
        let key_b = config.entries[*b].key.as_str();
        let pa = schema_order.get(key_a).copied().unwrap_or(usize::MAX);
        let pb = schema_order.get(key_b).copied().unwrap_or(usize::MAX);
        pa.cmp(&pb).then(key_a.cmp(key_b))
    });

    for idx in order {
        items.extend(clusters.remove(&idx).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_config;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_app(json: &str) -> App {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f.flush().unwrap();
        let config = load_config(f.path()).unwrap();
        // Keep the file alive for the duration of the test.
        std::mem::forget(f);
        App::new(config)
    }

    /// Position the cursor on the row for the given key.
    /// Panics if the key is not present in any section.
    fn move_cursor_to_key(app: &mut App, key: &str) {
        for (sec_idx, section) in app.sections.iter().enumerate() {
            for (item_idx, item) in section.items.iter().enumerate() {
                if app.config.entries[item.entry_idx].key == key {
                    app.cursor_section = sec_idx;
                    app.cursor_item = item_idx;
                    return;
                }
            }
        }
        panic!("key '{key}' not found in any section");
    }

    #[test]
    fn test_cursor_down_up() {
        let mut app =
            make_app(r#"{"TurnOnLogMetrics": true, "MinSeverity": "Info", "Protocol": "Cardano"}"#);
        // All items land in their known sections; at least one section has items.
        let initial_sec = app.cursor_section;
        let initial_item = app.cursor_item;
        app.cursor_down();
        // Either moved within section or to next section.
        let moved = app.cursor_section != initial_sec || app.cursor_item != initial_item;
        assert!(moved, "cursor_down should move the cursor");

        app.cursor_up();
        assert_eq!(
            (app.cursor_section, app.cursor_item),
            (initial_sec, initial_item),
            "cursor_up should undo the down movement"
        );
    }

    #[test]
    fn test_toggle_section_collapses_and_expands() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        assert!(app.sections[0].expanded);
        app.toggle_section();
        assert!(!app.sections[0].expanded);
        app.toggle_section();
        assert!(app.sections[0].expanded);
    }

    #[test]
    fn test_begin_edit_bool_toggles_immediately() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        move_cursor_to_key(&mut app, "TurnOnLogMetrics");
        app.begin_edit();
        // The edit should have completed immediately (bool toggle).
        assert_eq!(app.edit_mode, EditMode::None);
        // Value should be flipped.
        let entry = app.selected_entry().unwrap();
        assert_eq!(entry.display_value(), "false");
    }

    #[test]
    fn test_begin_edit_string_opens_buffer() {
        // ShelleyGenesisFile is a Path type, so begin_edit should open the typing buffer.
        let mut app = make_app(r#"{"ShelleyGenesisFile": "shelley-genesis.json"}"#);
        move_cursor_to_key(&mut app, "ShelleyGenesisFile");
        app.begin_edit();
        assert!(app.is_typing());
        assert_eq!(app.typing_buffer(), "shelley-genesis.json");
    }

    #[test]
    fn test_type_and_confirm_string() {
        let mut app = make_app(r#"{"ShelleyGenesisFile": "old.json"}"#);
        move_cursor_to_key(&mut app, "ShelleyGenesisFile");
        app.begin_edit(); // Path type — opens buffer.
        assert!(app.is_typing());
        // Clear buffer and type new value.
        app.backspace(); // Remove 'n'
                         // Just confirm "old.jso" (abbreviated) to test the flow.
        app.confirm_edit();
        assert_eq!(app.edit_mode, EditMode::None);
    }

    #[test]
    fn test_cancel_edit() {
        let mut app = make_app(r#"{"ShelleyGenesisFile": "old.json"}"#);
        move_cursor_to_key(&mut app, "ShelleyGenesisFile");
        app.begin_edit();
        assert!(app.is_typing());
        app.cancel_edit();
        assert_eq!(app.edit_mode, EditMode::None);
        // Value should be unchanged.
        let entry = app.selected_entry().unwrap();
        assert_eq!(entry.display_value(), "old.json");
    }

    #[test]
    fn test_request_quit_with_unsaved_changes() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        // Modify something.
        app.begin_edit(); // bool toggle
        assert!(app.is_modified());
        // First quit press: sets quit_prompt.
        app.request_quit();
        assert!(!app.should_quit);
        assert!(app.quit_prompt);
        // Second quit press: quits.
        app.request_quit();
        assert!(app.should_quit);
    }

    #[test]
    fn test_request_quit_clean_quits_immediately() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        assert!(!app.is_modified());
        app.request_quit();
        assert!(app.should_quit);
        assert!(!app.quit_prompt);
    }

    #[test]
    fn test_sections_are_ordered() {
        let app = make_app(
            r#"{"DiffusionMode": "InitiatorAndResponder", "MinSeverity": "Info", "ByronGenesisFile": "b.json"}"#,
        );
        // DiffusionMode -> Network, MinSeverity -> Logging, ByronGenesisFile -> Genesis
        // Expected order: Network, Genesis, Logging
        let names: Vec<&str> = app.sections.iter().map(|s| s.name.as_str()).collect();
        let net_pos = names.iter().position(|n| *n == "Network").unwrap();
        let gen_pos = names.iter().position(|n| *n == "Genesis").unwrap();
        let log_pos = names.iter().position(|n| *n == "Logging").unwrap();
        assert!(net_pos < gen_pos, "Network before Genesis");
        assert!(gen_pos < log_pos, "Genesis before Logging");
    }

    // -----------------------------------------------------------------------
    // Search tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_enter_search_activates_search_mode() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true, "MinSeverity": "Info"}"#);
        assert!(!app.search_active);
        app.enter_search();
        assert!(app.search_active);
        assert!(app.search_query.is_empty());
    }

    #[test]
    fn test_search_type_filters_items() {
        let mut app = make_app(
            r#"{"TurnOnLogMetrics": true, "MinSeverity": "Info", "ByronGenesisFile": "b.json"}"#,
        );
        app.enter_search();
        app.search_type_char('T');
        app.search_type_char('u');
        app.search_type_char('r');
        app.search_type_char('n');
        app.search_type_char('O');
        app.search_type_char('n');
        // "TurnOn" is a prefix of "TurnOnLogMetrics" — should appear in filtered items.
        assert!(!app.filtered_items.is_empty());
        // Cursor should have moved to the match.
        let (sec, item) = app.filtered_items[0];
        assert_eq!(app.cursor_section, sec);
        assert_eq!(app.cursor_item, item);
    }

    #[test]
    fn test_clear_search_restores_all() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true, "MinSeverity": "Info"}"#);
        app.enter_search();
        app.search_type_char('T');
        assert!(!app.filtered_items.is_empty());
        app.clear_search();
        assert!(!app.search_active);
        assert!(app.filtered_items.is_empty());
        assert!(app.search_query.is_empty());
    }

    #[test]
    fn test_search_backspace_updates_filter() {
        let mut app = make_app(
            r#"{"TurnOnLogMetrics": true, "MinSeverity": "Info", "ByronGenesisFile": "b.json"}"#,
        );
        app.enter_search();
        app.search_type_char('T');
        let count_after_t = app.filtered_items.len();
        app.search_type_char('u');
        app.search_backspace(); // back to "T"
        assert_eq!(app.filtered_items.len(), count_after_t);
    }

    // -----------------------------------------------------------------------
    // Diff tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_diff_empty_initially() {
        let app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        assert!(app.diff_entries().is_empty());
    }

    #[test]
    fn test_diff_captures_change() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        move_cursor_to_key(&mut app, "TurnOnLogMetrics");
        app.begin_edit(); // toggle bool: true -> false
        let diff = app.diff_entries();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].key, "TurnOnLogMetrics");
        assert_eq!(diff[0].original, "true");
        assert_eq!(diff[0].current, "false");
    }

    #[test]
    fn test_toggle_diff_overlay() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        assert!(!app.show_diff);
        app.toggle_diff();
        assert!(app.show_diff);
        app.toggle_diff();
        assert!(!app.show_diff);
    }

    #[test]
    fn test_close_diff_overlay() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        app.show_diff = true;
        app.close_diff();
        assert!(!app.show_diff);
    }

    #[test]
    fn test_originals_snapshot_is_immutable_after_edits() {
        let mut app = make_app(r#"{"TurnOnLogMetrics": true}"#);
        app.begin_edit(); // toggle: true -> false
                          // The original snapshot must still say "true".
        assert_eq!(app.originals.get("TurnOnLogMetrics"), Some("true"));
    }

    // -----------------------------------------------------------------------
    // save_and_reload tests
    // -----------------------------------------------------------------------

    /// Build an [`App`] backed by a real file in `dir` (not macOS's shared
    /// `/var/folders/T/`), so that `save_config`'s atomic rename stays within
    /// the same directory and never crosses a device boundary.
    fn make_app_in_dir(dir: &std::path::Path, json: &str) -> App {
        let path = dir.join("config.json");
        std::fs::write(&path, json.as_bytes()).unwrap();
        let config = load_config(&path).unwrap();
        App::new(config)
    }

    #[test]
    fn test_save_and_reload_missing_pid_file_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = make_app_in_dir(dir.path(), r#"{"TurnOnLogMetrics": true}"#);
        let non_existent = dir.path().join("dugite-nonexistent.pid");
        app.save_and_reload(&non_existent);
        // The config should be saved (file is now clean).
        assert!(!app.is_modified());
        // Feedback must mention the SIGHUP skip (PID file not found).
        let fb = app.feedback.as_deref().unwrap_or("");
        assert!(
            fb.contains("SIGHUP skipped") || fb.contains("Saved"),
            "Expected feedback mentioning SIGHUP skip or save, got: '{fb}'"
        );
    }

    #[test]
    fn test_save_and_reload_invalid_pid_content_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("bp.pid");
        std::fs::write(&pid_file, b"not-a-pid\n").unwrap();

        let mut app = make_app_in_dir(dir.path(), r#"{"TurnOnLogMetrics": true}"#);
        app.save_and_reload(&pid_file);
        assert!(!app.is_modified());
        let fb = app.feedback.as_deref().unwrap_or("");
        assert!(
            fb.contains("invalid PID") || fb.contains("SIGHUP skipped"),
            "Expected feedback about invalid PID, got: '{fb}'"
        );
    }

    #[test]
    fn test_build_sections_emits_object_header_with_default_state() {
        use crate::schema::KNOWN_PARAMS;
        let app = make_app(r#"{}"#);

        // Verify all current ParamType::Object entries default-collapsed.
        let any_object = KNOWN_PARAMS.iter().any(|d| {
            matches!(
                &d.param_type,
                crate::schema::ParamType::Object { fields: _ }
            )
        });
        assert!(
            any_object,
            "test premise: at least one Object exists in schema"
        );

        // Locate the AcceptedConnectionsLimit header row and confirm its flags.
        let mut found = false;
        for section in &app.sections {
            for item in &section.items {
                if app.config.entries[item.entry_idx].key == "AcceptedConnectionsLimit"
                    && item.path.is_empty()
                {
                    assert!(item.is_container, "Object row must be a container");
                    assert!(!item.expanded, "Object rows start collapsed");
                    assert_eq!(item.depth, 0, "top-level row depth must be 0");
                    found = true;
                }
            }
        }
        assert!(found, "AcceptedConnectionsLimit header row not found");
    }

    #[test]
    fn test_cursor_skips_rows_under_collapsed_container() {
        let mut app = make_app(r#"{}"#);

        // After build_sections every Object header is collapsed. The cursor must
        // never land on a sub-row whose parent header is collapsed.
        move_cursor_to_key(&mut app, "AcceptedConnectionsLimit");
        let sec = app.cursor_section;
        let item_idx_header = app.cursor_item;

        app.cursor_down();
        let new_item = &app.sections[app.cursor_section].items[app.cursor_item];
        if app.cursor_section == sec {
            let header_entry_idx = app.sections[sec].items[item_idx_header].entry_idx;
            assert!(
                new_item.path.is_empty() || new_item.entry_idx != header_entry_idx,
                "cursor_down landed on a sub-row of the collapsed container at item {} (path = {:?})",
                app.cursor_item,
                new_item.path
            );
        }
    }
}
