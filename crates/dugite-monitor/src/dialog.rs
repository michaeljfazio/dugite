//! Themed ratatui modal for selecting which dugite-node to attach to.
//!
//! Two entry points share the same drawing code:
//!
//! - [`run`] — standalone, used at startup before the main TUI is set up.
//!   Owns the terminal lifecycle (raw mode + alternate screen).
//! - [`select_with_terminal`] — runs inside the main TUI loop. Reuses the
//!   caller's already-configured `Terminal`, so the dashboard underneath
//!   is preserved and the dialog appears as an overlay on top.
//!
//! The dialog inherits the active [`Theme`] so it matches the rest of the
//! dashboard (border, accent, highlight, footer-style key hints).

use std::io;
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Terminal;

use crate::discover::DiscoveredNode;
use crate::theme::Theme;

/// Standalone variant: show the selection dialog at startup.
///
/// Enables raw mode + alternate screen on entry, tears them down on every
/// exit path (including errors) so the caller does not need cleanup
/// boilerplate. Returns the chosen `metrics_url`, or `None` if the user
/// quit (q / Esc / Ctrl-C).
pub fn run(nodes: &[DiscoveredNode], theme: &Theme) -> Result<Option<String>> {
    if nodes.is_empty() {
        return Err(anyhow!("dialog::run called with empty node list"));
    }

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let result = Terminal::new(backend)
        .map_err(anyhow::Error::from)
        .and_then(|mut terminal| -> Result<Option<String>> {
            terminal.clear()?;
            event_loop(&mut terminal, nodes, theme)
        });
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
    result
}

/// In-loop variant: render the selection dialog over the existing TUI.
///
/// The caller must already have raw mode enabled and own the terminal.
/// On return the underlying terminal is untouched (no clear, no leave-
/// alternate-screen) so the dashboard underneath is restored on the next
/// `terminal.draw(...)` cycle.
///
/// `nodes` may be empty — in that case an informational popup is shown
/// instead, with a single-key dismiss path. Returns `None` if the user
/// dismissed the dialog without picking a node.
pub fn select_with_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    nodes: &[DiscoveredNode],
    theme: &Theme,
) -> Result<Option<String>> {
    event_loop(terminal, nodes, theme)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    nodes: &[DiscoveredNode],
    theme: &Theme,
) -> Result<Option<String>> {
    let mut cursor: usize = 0;

    loop {
        terminal.draw(|frame| draw(frame, nodes, cursor, theme))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if nodes.is_empty() {
                    // Empty-state popup: any key dismisses.
                    return Ok(None);
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') if cursor > 0 => {
                        cursor -= 1;
                    }
                    KeyCode::Down | KeyCode::Char('j') if cursor + 1 < nodes.len() => {
                        cursor += 1;
                    }
                    KeyCode::Home => {
                        cursor = 0;
                    }
                    KeyCode::End => {
                        cursor = nodes.len() - 1;
                    }
                    KeyCode::Enter => {
                        return Ok(Some(nodes[cursor].metrics_url.clone()));
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        return Ok(None);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(frame: &mut Frame, nodes: &[DiscoveredNode], cursor: usize, theme: &Theme) {
    if nodes.is_empty() {
        draw_empty(frame, theme);
        return;
    }
    draw_selection(frame, nodes, cursor, theme);
}

fn draw_selection(frame: &mut Frame, nodes: &[DiscoveredNode], cursor: usize, theme: &Theme) {
    // Size the dialog to fit the content. Each node uses 3 lines (row, detail,
    // gap); plus 1 header line + 1 hint line + 2 borders + 2 vertical padding.
    let content_h = 1 + (nodes.len() as u16 * 3).saturating_sub(1).max(1) + 2;
    let want_h: u16 = content_h + 4; // +4 for borders + padding
    let want_w: u16 = 76;
    let area = centered_rect(want_w, want_h, frame.area());

    // Paint a clean background so the panel reads on its own.
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .style(Style::default().bg(theme.bg).fg(theme.fg))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Select dugite-node",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header_count = nodes.len();
    let header = Line::from(vec![
        Span::styled(
            format!("{header_count} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if header_count == 1 {
                "node discovered. Press "
            } else {
                "nodes discovered. Press "
            },
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            "Enter",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" to attach.", Style::default().fg(theme.muted)),
    ]);

    let mut lines: Vec<Line> = Vec::with_capacity(header_count * 3 + 2);
    lines.push(header);
    lines.push(Line::from(""));

    for (i, node) in nodes.iter().enumerate() {
        let selected = i == cursor;
        let cursor_glyph = if selected { "▸ " } else { "  " };
        let cursor_color = if selected { theme.accent } else { theme.muted };

        let network = node
            .network
            .map(|n| n.label().to_string())
            .unwrap_or_else(|| "--".to_string());
        let role = node.role_label();
        let era = era_label(node.protocol_major_version);
        let tip = node
            .tip_slot
            .map_or_else(|| "--".to_string(), format_with_commas);
        let sync = node
            .sync_progress_percent
            .map_or_else(|| "--".to_string(), |p| format!("{p:.1}%"));

        let value_style = if selected {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };

        // Row 1: cursor + network/role/era/tip/sync.
        let primary = Line::from(vec![
            Span::styled(cursor_glyph, Style::default().fg(cursor_color)),
            Span::styled(
                format!("{network:<9}"),
                styled_network(network.as_str(), theme, selected),
            ),
            Span::styled(format!("{role:<6}"), styled_role(role, theme, selected)),
            Span::styled(format!("{era:<9}"), value_style),
            Span::styled("tip ", Style::default().fg(theme.muted)),
            Span::styled(format!("{tip:>14}"), value_style),
            Span::styled("  sync ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("{sync:>7}"),
                styled_sync(node.sync_progress_percent, theme, selected),
            ),
        ]);
        lines.push(primary);

        // Row 2: detail line (pid, port, db).
        let port = port_of_url(&node.metrics_url).unwrap_or(0);
        let db = node
            .db_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--".to_string());
        let detail = Line::from(vec![
            Span::raw("    "),
            Span::styled("pid ", Style::default().fg(theme.muted)),
            Span::styled(node.pid.to_string(), Style::default().fg(theme.info)),
            Span::styled("  port ", Style::default().fg(theme.muted)),
            Span::styled(port.to_string(), Style::default().fg(theme.info)),
            Span::styled("  db ", Style::default().fg(theme.muted)),
            Span::styled(truncate_path(&db, 38), Style::default().fg(theme.fg)),
        ]);
        lines.push(detail);

        if i + 1 < header_count {
            lines.push(Line::from(""));
        }
    }

    // Footer hint, separated by a blank line.
    lines.push(Line::from(""));
    lines.push(footer_hint(theme));

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_empty(frame: &mut Frame, theme: &Theme) {
    let want_w: u16 = 60;
    let want_h: u16 = 9;
    let area = centered_rect(want_w, want_h, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.warning))
        .style(Style::default().bg(theme.bg).fg(theme.fg))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "No dugite-node found",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            "No running dugite-node processes were discovered.",
            Style::default().fg(theme.fg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Start a node and try again, or continue with the",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            "currently attached endpoint.",
            Style::default().fg(theme.muted),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(theme.muted)),
            Span::styled(
                "any key",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to dismiss.", Style::default().fg(theme.muted)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

fn footer_hint(theme: &Theme) -> Line<'static> {
    let key = |k: &'static str| {
        Span::styled(
            format!("[{k}]"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let text = |t: &'static str| Span::styled(t, Style::default().fg(theme.muted));
    Line::from(vec![
        key("↑↓"),
        text(" Move  "),
        key("Enter"),
        text(" Attach  "),
        key("q"),
        text(" Cancel"),
    ])
}

// ---------------------------------------------------------------------------
// Styling helpers
// ---------------------------------------------------------------------------

fn styled_network(label: &str, theme: &Theme, selected: bool) -> Style {
    let base = match label {
        "Mainnet" => theme.success,
        "Preview" | "Preprod" => theme.accent,
        "Guild" => theme.info,
        _ => theme.muted,
    };
    let mut style = Style::default().fg(base);
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

fn styled_role(role: &str, theme: &Theme, selected: bool) -> Style {
    let base = match role {
        "bp" => theme.warning,
        "relay" => theme.info,
        _ => theme.muted,
    };
    let mut style = Style::default().fg(base);
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

fn styled_sync(pct: Option<f64>, theme: &Theme, selected: bool) -> Style {
    let base = match pct {
        Some(p) if p >= 99.9 => theme.success,
        Some(p) if p >= 50.0 => theme.warning,
        Some(_) => theme.error,
        None => theme.muted,
    };
    let mut style = Style::default().fg(base);
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

fn centered_rect(want_w: u16, want_h: u16, r: Rect) -> Rect {
    let w = want_w.min(r.width);
    let h = want_h.min(r.height);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(r.height.saturating_sub(h) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(r);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(r.width.saturating_sub(w) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn era_label(pv: Option<u64>) -> &'static str {
    match pv {
        Some(0..=1) => "Byron",
        Some(2..=3) => "Shelley",
        Some(4) => "Allegra",
        Some(5) => "Mary",
        Some(6) => "Alonzo",
        Some(7) => "Babbage",
        Some(_) => "Conway",
        None => "--",
    }
}

fn port_of_url(url: &str) -> Option<u16> {
    let after_scheme = url.split_once("://")?.1;
    let host_port = after_scheme
        .split_once('/')
        .map(|p| p.0)
        .unwrap_or(after_scheme);
    let port_str = host_port.rsplit_once(':')?.1;
    port_str.parse().ok()
}

/// Format a u64 with thousands separators ("111661041" → "111,661,041").
fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch as char);
    }
    out
}

/// Shorten an absolute path for display, keeping the trailing component(s).
///
/// "/home/me/dugite/db-preview/ledger/snapshot" with max=20 → "…/ledger/snapshot".
fn truncate_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    let tail = &path[path.len() - (max.saturating_sub(1))..];
    // Try to align on a path separator so we don't break in the middle of a
    // component when possible.
    if let Some(idx) = tail.find('/') {
        format!("…{}", &tail[idx..])
    } else {
        format!("…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_of_url_parses_standard() {
        assert_eq!(port_of_url("http://127.0.0.1:12798/metrics"), Some(12798));
        assert_eq!(port_of_url("http://localhost:12796"), Some(12796));
    }

    #[test]
    fn port_of_url_returns_none_when_missing() {
        assert_eq!(port_of_url("http://localhost/metrics"), None);
    }

    #[test]
    fn era_label_maps_major_versions() {
        assert_eq!(era_label(Some(0)), "Byron");
        assert_eq!(era_label(Some(1)), "Byron");
        assert_eq!(era_label(Some(2)), "Shelley");
        assert_eq!(era_label(Some(7)), "Babbage");
        assert_eq!(era_label(Some(11)), "Conway");
        assert_eq!(era_label(None), "--");
    }

    #[test]
    fn format_with_commas_works() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(42), "42");
        assert_eq!(format_with_commas(1_000), "1,000");
        assert_eq!(format_with_commas(111_661_041), "111,661,041");
    }

    #[test]
    fn truncate_path_keeps_tail() {
        let p = "/home/me/dugite/db-preview/ledger/snapshot";
        let t = truncate_path(p, 20);
        assert!(t.starts_with('…'), "expected ellipsis prefix, got {t:?}");
        assert!(
            t.len() <= 22,
            "truncated path should be <= max + ellipsis len"
        );
    }

    #[test]
    fn truncate_path_passthrough_when_short() {
        assert_eq!(truncate_path("/db", 20), "/db");
    }
}
