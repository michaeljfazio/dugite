//! Pre-launch ratatui modal for selecting which dugite-node to attach
//! to when multiple have been discovered.
//!
//! Run *before* the main metrics loop enters its render cycle. Uses
//! the same `Terminal` + alternate-screen setup so we do not tear down
//! and rebuild the terminal between this modal and the main UI.

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
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

use crate::discover::DiscoveredNode;

/// Show the selection dialog. Returns the chosen `metrics_url`, or
/// `None` if the user quit (q / Esc / Ctrl-C).
///
/// Owns its own terminal lifecycle: enables raw mode + alternate
/// screen on entry and tears them down on every exit path (including
/// errors), so the caller does not need to wrap the call in cleanup
/// boilerplate.
pub fn run(nodes: &[DiscoveredNode]) -> Result<Option<String>> {
    if nodes.is_empty() {
        return Err(anyhow!("dialog::run called with empty node list"));
    }

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let result = run_inner(nodes);
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
    result
}

fn run_inner(nodes: &[DiscoveredNode]) -> Result<Option<String>> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut cursor: usize = 0;

    loop {
        terminal.draw(|frame| draw(frame, nodes, cursor))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up if cursor > 0 => {
                        cursor -= 1;
                    }
                    KeyCode::Down if cursor + 1 < nodes.len() => {
                        cursor += 1;
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

fn draw(frame: &mut Frame, nodes: &[DiscoveredNode], cursor: usize) {
    let area = centered_rect(80, 60, frame.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Dugite Monitor — Select a node ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = "Multiple dugite-node processes found. Select one:";
    let footer = "↑/↓ select   Enter attach   q quit";

    let mut lines: Vec<Line> = Vec::with_capacity(nodes.len() * 3 + 3);
    lines.push(Line::from(header));
    lines.push(Line::from(""));

    for (i, node) in nodes.iter().enumerate() {
        let cursor_char = if i == cursor { "▸" } else { " " };
        let style = if i == cursor {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let network = node
            .network
            .map(|n| n.label().to_string())
            .unwrap_or_else(|| "--".to_string());
        let role = node.role_label();
        let era = era_label(node.protocol_major_version);
        let tip = node.tip_slot.map_or("--".to_string(), format_with_commas);
        let sync = node
            .sync_progress_percent
            .map_or("--".to_string(), |p| format!("{p:.1}%"));

        lines.push(Line::from(vec![Span::styled(
            format!("{cursor_char} {network:<8} {role:<5} {era:<8} tip {tip:<14} sync {sync}"),
            style,
        )]));

        let db = node
            .db_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--".to_string());
        lines.push(Line::from(vec![Span::raw(format!(
            "    pid {}  port {}  db {}",
            node.pid,
            port_of_url(&node.metrics_url).unwrap_or(0),
            db,
        ))]));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(footer));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
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
}
