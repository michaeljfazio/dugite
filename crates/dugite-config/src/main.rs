//! dugite-config — Interactive TUI configuration editor for Dugite node config files.
//!
//! # Subcommands
//!
//! | Subcommand | Description                                               |
//! |------------|-----------------------------------------------------------|
//! | edit       | Open a config file in the interactive TUI editor          |
//! | init       | Generate a default config for a named network             |
//! | validate   | Validate a config file against the parameter schema       |
//! | get        | Print the value of a single parameter                     |
//! | set        | Update the value of a single parameter in a config file   |
//!
//! # Interactive editor key bindings
//!
//! | Key          | Action                                         |
//! |--------------|------------------------------------------------|
//! | j / Down     | Move cursor down                               |
//! | k / Up       | Move cursor up                                 |
//! | Enter        | Edit selected parameter (toggle bool / cycle   |
//! |              | enum / open text buffer for string/number/path)|
//! | Esc          | Cancel current edit / close search / close diff|
//! | Tab          | Collapse / expand current section              |
//! | /            | Enter search mode (fuzzy filter)               |
//! | Ctrl+D       | Show diff overlay (original vs. current)       |
//! | Ctrl+S       | Save config to disk (creates .bak backup)      |
//! | Ctrl+R       | Save & send SIGHUP to running node (live reload)|
//! | q            | Quit (prompts if there are unsaved changes)    |
//!
//! # Two-panel layout (>=80 columns)
//!
//! Left 60%:  parameter tree — sections, keys, right-aligned values.
//! Right 40%: description panel — type, default, tuning hint, docs for selected parameter.
//!
//! Below 80 columns the right panel is hidden and the tree fills the terminal.

mod app;
mod config;
mod diff;
mod discover;
mod path;
mod schema;
mod search;
mod selector;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;

use app::App;
use config::load_config;
use schema::{build_lookup, Network};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Interactive TUI configuration editor for Dugite Cardano node config files.
#[derive(Parser, Debug)]
#[command(
    name = "dugite-config",
    version,
    about = "Interactive TUI editor for Dugite node configuration files"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available sub-commands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Open a configuration file in the interactive TUI editor.
    ///
    /// When invoked without `<config_file>`, enumerates running `dugite-node`
    /// instances on the local machine and either auto-attaches (if exactly
    /// one is running) or presents a selector (if more than one). The
    /// discovered process's OS PID is then used for Ctrl+R live reload —
    /// no PID file required.
    Edit {
        /// Path to the Cardano node configuration JSON file. Optional —
        /// omit to auto-discover running `dugite-node` instances instead.
        config_file: Option<PathBuf>,

        /// Path to a file containing the running dugite-node PID.
        ///
        /// Used by Ctrl+R ("Save & Reload") to send SIGHUP to the live node.
        /// If this file does not exist when Ctrl+R is pressed, the config is
        /// still saved but the SIGHUP is skipped with a clear error message.
        ///
        /// Ignored when `<config_file>` is omitted — the discovered process's
        /// OS PID is used directly.
        #[arg(long)]
        node_pid_file: Option<PathBuf>,
    },

    /// Generate a default configuration file for the given network.
    ///
    /// Writes a JSON config with sensible defaults to the specified output
    /// path (or stdout if `--out` is omitted).  Genesis file paths use the
    /// conventional `<network>-*-genesis.json` naming relative to the config.
    Init {
        /// Target network: mainnet, preview, or preprod.
        #[arg(long, short)]
        network: String,

        /// Output path for the generated config file.  Prints to stdout if
        /// omitted.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Validate a configuration file against the parameter schema.
    ///
    /// Exits with code 0 if the file is valid, 1 if it contains errors.
    /// Suitable for use in CI/CD pipelines.
    Validate {
        /// Path to the Cardano node configuration JSON file to validate.
        config_file: PathBuf,
    },

    /// Print the current value of a single parameter.
    Get {
        /// The JSON key name to read (e.g. "EnableP2P").
        key: String,

        /// Path to the configuration file.
        #[arg(long, short)]
        config: PathBuf,

        /// Also print the parameter's description and type.
        #[arg(long, short)]
        verbose: bool,
    },

    /// Update the value of a single parameter in a config file.
    ///
    /// Creates a `.bak` backup of the original file before writing.
    Set {
        /// The JSON key name to update (e.g. "MinSeverity").
        key: String,

        /// The new value as a string (booleans: "true"/"false", numbers as
        /// decimal, strings and paths as plain text).
        value: String,

        /// Path to the configuration file.
        #[arg(long, short)]
        config: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Edit {
            config_file,
            node_pid_file,
        } => {
            run_edit(config_file, node_pid_file)?;
        }
        Commands::Init { network, out } => {
            run_init(&network, out.as_deref())?;
        }
        Commands::Validate { config_file } => {
            run_validate(&config_file)?;
        }
        Commands::Get {
            key,
            config,
            verbose,
        } => {
            run_get(&key, &config, verbose)?;
        }
        Commands::Set { key, value, config } => {
            run_set(&key, &value, &config)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `edit` subcommand — interactive TUI
// ---------------------------------------------------------------------------

/// How Ctrl+R should locate the dugite-node PID for SIGHUP delivery.
#[derive(Debug, Clone)]
enum ReloadTarget {
    /// Read the PID from a file at the given path on each Ctrl+R press.
    PidFile(PathBuf),
    /// Use the given OS PID directly (discovered at startup via sysinfo).
    Pid(u32),
}

/// Dispatcher for `Commands::Edit`. When `config_file` is `Some`, behaves
/// exactly like the legacy explicit-args invocation. When `None`, enumerates
/// running `dugite-node` instances and either auto-attaches (if exactly one)
/// or presents a selector (if more than one); errors out cleanly if none are
/// running.
fn run_edit(config_file: Option<PathBuf>, node_pid_file: Option<PathBuf>) -> Result<()> {
    if let Some(path) = config_file {
        // Explicit-args mode: preserve legacy default for the PID file so
        // existing operator workflows (the bp-pair launcher) keep working.
        let pid_file = node_pid_file.unwrap_or_else(|| PathBuf::from("./logs/bp-pair/bp.pid"));
        return run_editor(&path, ReloadTarget::PidFile(pid_file));
    }

    // Discovery mode.
    let nodes = discover::discover_dugite_nodes();
    let chosen = match nodes.len() {
        0 => {
            anyhow::bail!(
                "No running dugite-node instances found. \
                 Pass an explicit <config_file> to edit a config without attaching."
            );
        }
        1 => nodes.into_iter().next().unwrap(),
        _ => {
            let selectable_count = nodes.iter().filter(|n| n.is_selectable()).count();
            if selectable_count == 0 {
                anyhow::bail!(
                    "Discovered {} running dugite-node instance(s) but none have a \
                     readable config file. Pass an explicit <config_file> to attach manually.",
                    nodes.len()
                );
            }
            match selector::select(nodes)? {
                Some(n) => n,
                None => {
                    eprintln!("Cancelled.");
                    return Ok(());
                }
            }
        }
    };

    if !chosen.is_selectable() {
        anyhow::bail!(
            "Discovered dugite-node pid={} has no readable config at '{}'.",
            chosen.pid,
            chosen.config_path.display()
        );
    }

    eprintln!(
        "Attaching to dugite-node pid={} config={}",
        chosen.pid,
        chosen.config_path.display()
    );
    run_editor(&chosen.config_path, ReloadTarget::Pid(chosen.pid))
}

/// Load `path`, set up the terminal, run the event loop, restore terminal.
fn run_editor(path: &Path, reload: ReloadTarget) -> Result<()> {
    let config =
        load_config(path).with_context(|| format!("loading config file '{}'", path.display()))?;

    let mut app = App::new(config);

    // Set up the terminal in raw alternate-screen mode.
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_loop(&mut terminal, &mut app, &reload);

    // Restore terminal unconditionally, even on error.
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);

    result
}

/// Main event / render loop.
///
/// Renders a frame on every iteration, then waits up to 100 ms for a key
/// event.  Returns when `app.should_quit` is set.
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    reload: &ReloadTarget,
) -> Result<()> {
    loop {
        // Render current state.
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Consume the feedback message after the first render so it is shown
        // for exactly one frame.
        let _feedback_shown = app.feedback.take();

        if app.should_quit {
            return Ok(());
        }

        // Wait at most 100 ms for a key event (keeps the UI responsive).
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(app, key.code, key.modifiers, reload);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key handler
// ---------------------------------------------------------------------------

/// Dispatch a key press to the appropriate [`App`] action.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers, reload: &ReloadTarget) {
    // Ctrl+S saves in any mode.
    if code == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
        app.save();
        return;
    }

    // Ctrl+R saves and sends SIGHUP to the running node (live reload).
    if code == KeyCode::Char('r') && modifiers.contains(KeyModifiers::CONTROL) {
        match reload {
            ReloadTarget::PidFile(p) => app.save_and_reload(p),
            ReloadTarget::Pid(pid) => app.save_and_signal_pid(*pid),
        }
        return;
    }

    // Ctrl+D toggles the diff overlay (not available while typing or searching).
    if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
        if !app.is_typing() && !app.search_active {
            app.toggle_diff();
        }
        return;
    }

    // If the diff overlay is showing, only Esc closes it.
    if app.show_diff {
        if code == KeyCode::Esc {
            app.close_diff();
        }
        return;
    }

    // Search mode dispatches to its own handler.
    if app.search_active {
        handle_search(app, code);
        return;
    }

    if app.is_typing() {
        handle_typing(app, code);
    } else {
        handle_browse(app, code);
    }
}

/// Handle key events while in search mode.
fn handle_search(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.clear_search();
        }
        KeyCode::Backspace => {
            app.search_backspace();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor_down();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor_up();
        }
        KeyCode::Enter => {
            // Confirm the search: leave search mode with the cursor on the
            // first match (which is already there), and return to browse.
            app.clear_search();
        }
        KeyCode::Char(c) => {
            app.search_type_char(c);
        }
        _ => {}
    }
}

/// Handle key events while in typing / edit mode.
fn handle_typing(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Enter => {
            app.confirm_edit();
        }
        KeyCode::Esc => {
            app.cancel_edit();
        }
        KeyCode::Backspace => {
            app.backspace();
        }
        KeyCode::Char(c) => {
            app.type_char(c);
        }
        _ => {}
    }
}

/// Handle key events while in browse / navigation mode.
fn handle_browse(app: &mut App, code: KeyCode) {
    match code {
        // Navigation.
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor_down();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor_up();
        }

        // Enter edit mode (or toggle/cycle).
        KeyCode::Enter | KeyCode::Char('e') => {
            app.begin_edit();
        }

        // Collapse / expand section.
        KeyCode::Tab => {
            app.toggle_section();
        }

        // Enter search mode.
        KeyCode::Char('/') => {
            app.enter_search();
        }

        // Quit.
        KeyCode::Char('q') | KeyCode::Esc => {
            app.request_quit();
        }

        _ => {}
    }
}

// ---------------------------------------------------------------------------
// `init` subcommand
// ---------------------------------------------------------------------------

/// Generate a default config file for the named network.
fn run_init(network_str: &str, out: Option<&Path>) -> Result<()> {
    let network = Network::from_str(network_str).with_context(|| {
        format!(
            "unknown network '{}' — valid values are: mainnet, preview, preprod",
            network_str
        )
    })?;

    let map = schema::network_defaults(network);
    let json = serde_json::Value::Object(map);
    let mut pretty =
        serde_json::to_string_pretty(&json).context("serialising default config to JSON")?;
    pretty.push('\n');

    match out {
        Some(path) => {
            std::fs::write(path, &pretty)
                .with_context(|| format!("writing config to '{}'", path.display()))?;
            eprintln!(
                "Wrote default {} config to '{}'",
                network_str,
                path.display()
            );
        }
        None => {
            print!("{pretty}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `validate` subcommand
// ---------------------------------------------------------------------------

/// Validate a config file against the parameter schema.
///
/// Validation rules:
/// - Must be a valid JSON object.
/// - Every known key must have a value that passes [`schema::ParamType::validate`].
/// - Unknown keys are reported as warnings (not errors).
fn run_validate(path: &Path) -> Result<()> {
    let loaded =
        load_config(path).with_context(|| format!("loading config file '{}'", path.display()))?;

    let lookup = build_lookup();
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for entry in &loaded.entries {
        match lookup.get(entry.key.as_str()) {
            Some(def) => {
                // Known parameter — validate the value.
                let raw = entry.display_value();
                if let Err(msg) = def.param_type.validate(&raw) {
                    errors.push(format!("  '{}': {}", entry.key, msg));
                }
            }
            None => {
                warnings.push(format!(
                    "  '{}': unknown parameter (not in schema)",
                    entry.key
                ));
            }
        }
    }

    if !warnings.is_empty() {
        eprintln!("Warnings:");
        for w in &warnings {
            eprintln!("{w}");
        }
    }

    if errors.is_empty() {
        eprintln!(
            "OK — '{}' is valid ({} parameters, {} unknown).",
            path.display(),
            loaded.entries.len(),
            warnings.len()
        );
        Ok(())
    } else {
        eprintln!("Errors:");
        for e in &errors {
            eprintln!("{e}");
        }
        // Non-zero exit via anyhow bail.
        anyhow::bail!(
            "'{}' failed validation: {} error(s)",
            path.display(),
            errors.len()
        );
    }
}

// ---------------------------------------------------------------------------
// `get` subcommand
// ---------------------------------------------------------------------------

/// Print the current value of a single parameter.
fn run_get(key: &str, path: &Path, verbose: bool) -> Result<()> {
    let loaded =
        load_config(path).with_context(|| format!("loading config file '{}'", path.display()))?;

    let entry = loaded
        .entries
        .iter()
        .find(|e| e.key == key)
        .with_context(|| format!("key '{}' not found in '{}'", key, path.display()))?;

    if verbose {
        let lookup = build_lookup();
        if let Some(def) = lookup.get(key) {
            println!("Key:         {}", def.key);
            println!("Type:        {}", def.param_type.label());
            if !def.default.is_empty() {
                println!("Default:     {}", def.default);
            }
            println!("Section:     {}", def.section);
            println!("Description: {}", def.description);
            if !def.tuning_hint.is_empty() {
                println!("Hint:        {}", def.tuning_hint);
            }
            println!();
        }
    }

    println!("{}", entry.display_value());
    Ok(())
}

// ---------------------------------------------------------------------------
// `set` subcommand
// ---------------------------------------------------------------------------

/// Update the value of a single parameter in a config file.
///
/// If the key is in the schema but missing from the file, a new entry is
/// appended (typed from the schema default so `apply_edit` coerces correctly).
/// If the key is in neither the file nor the schema, the call is rejected.
fn run_set(key: &str, value: &str, path: &Path) -> Result<()> {
    let mut loaded =
        load_config(path).with_context(|| format!("loading config file '{}'", path.display()))?;

    let lookup = build_lookup();
    let def = lookup.get(key).copied();

    // Validate the value against the schema if the key is known.
    if let Some(def) = def {
        def.param_type
            .validate(value)
            .map_err(|msg| anyhow::anyhow!("invalid value for '{}': {}", key, msg))?;
    }

    let existing_pos = loaded.entries.iter().position(|e| e.key == key);
    let entry_pos = match existing_pos {
        Some(pos) => pos,
        None => {
            let def = def.with_context(|| {
                format!(
                    "key '{}' not found in '{}' and not present in the schema",
                    key,
                    path.display()
                )
            })?;
            // Seed the new entry with the typed schema default so `apply_edit`
            // coerces the user's value to the right JSON type (bool, number,
            // string, ...). For schema entries without a representable default,
            // fall back to a JSON string.
            let seed_value = def
                .default_as_json()
                .unwrap_or_else(|| serde_json::Value::String(String::new()));
            loaded.entries.push(config::ConfigEntry {
                key: key.to_string(),
                value: seed_value,
                modified: false,
                present_in_file: true,
                synthetic_paths: std::collections::HashSet::new(),
            });
            loaded.entries.len() - 1
        }
    };

    loaded.entries[entry_pos]
        .apply_edit(value)
        .with_context(|| format!("applying value '{}' to key '{}'", value, key))?;

    // Save (creates .bak backup automatically).
    config::save_config(&mut loaded)
        .with_context(|| format!("saving config file '{}'", path.display()))?;

    eprintln!("Set '{}' = '{}' in '{}'", key, value, path.display());
    Ok(())
}
