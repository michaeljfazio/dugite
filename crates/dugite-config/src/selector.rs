//! Interactive list selector for picking among multiple discovered
//! `dugite-node` instances when `dugite-config edit` is invoked without an
//! explicit `<config_file>` argument.
//!
//! Layout: a single bordered list with one row per discovered node, columns
//! aligned PID / PORT / SOCKET / CONFIG. Rows whose config file is missing or
//! permission-denied are rendered dimmed and cannot be selected.
//!
//! Key bindings:
//! * `j` / `Down`  — cursor down
//! * `k` / `Up`    — cursor up
//! * `Enter`       — pick the highlighted row (only if selectable)
//! * `q` / `Esc`   — cancel and exit without picking
//!
//! Returns `Ok(Some(node))` on pick, `Ok(None)` on cancel.

use std::io;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Layout},
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::discover::{ConfigSource, ConfigStatus, RunningNode};

/// Render-only formatting of a node row into PID/PORT/SOCKET/CONFIG columns.
/// Pulled out so the formatting can be unit-tested without setting up a TTY.
fn format_row(n: &RunningNode) -> [String; 4] {
    let pid = n.pid.to_string();
    let port = n.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
    let socket = n
        .socket
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".into());

    let mut config = n.config_path.display().to_string();
    if matches!(n.config_source, ConfigSource::Default) {
        config.push_str(" (default)");
    }
    match n.status {
        ConfigStatus::Missing => config.push_str(" [missing]"),
        ConfigStatus::PermissionDenied => config.push_str(" [permission denied]"),
        ConfigStatus::Ok => {}
    }
    [pid, port, socket, config]
}

/// Run an interactive selector over `nodes` and return the chosen one, or
/// `None` if the operator pressed `q` / Esc.
pub fn select(nodes: Vec<RunningNode>) -> io::Result<Option<RunningNode>> {
    if nodes.is_empty() {
        return Ok(None);
    }

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_selector_loop(&mut terminal, &nodes);

    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);

    result.map(|opt_idx| opt_idx.map(|i| nodes[i].clone()))
}

fn run_selector_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    nodes: &[RunningNode],
) -> io::Result<Option<usize>> {
    let rows: Vec<[String; 4]> = nodes.iter().map(format_row).collect();

    // Column widths derived once from the data so columns line up.
    let col_widths = column_widths(&rows);

    // Initial cursor: first selectable row, or 0 if none are selectable.
    let mut state = ListState::default();
    state.select(Some(
        nodes
            .iter()
            .position(RunningNode::is_selectable)
            .unwrap_or(0),
    ));

    loop {
        terminal.draw(|frame| draw_selector(frame, nodes, &rows, &col_widths, &mut state))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                KeyCode::Down | KeyCode::Char('j') => {
                    move_cursor(&mut state, nodes.len(), 1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    move_cursor(&mut state, nodes.len(), -1);
                }
                KeyCode::Enter => {
                    if let Some(i) = state.selected() {
                        if nodes[i].is_selectable() {
                            return Ok(Some(i));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn move_cursor(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let new = (cur + delta).rem_euclid(len as i32) as usize;
    state.select(Some(new));
}

fn column_widths(rows: &[[String; 4]]) -> [usize; 4] {
    let headers = ["PID", "PORT", "SOCKET", "CONFIG"];
    let mut widths = headers.map(str::len);
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    widths
}

fn draw_selector(
    frame: &mut Frame,
    nodes: &[RunningNode],
    rows: &[[String; 4]],
    widths: &[usize; 4],
    state: &mut ListState,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Title.
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!("Select a running dugite-node ({} discovered)", nodes.len()),
            Style::default().add_modifier(Modifier::BOLD),
        )])),
        chunks[0],
    );

    // Header.
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format_columns(["PID", "PORT", "SOCKET", "CONFIG"], widths),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )])),
        chunks[1],
    );

    // List body.
    let items: Vec<ListItem> = rows
        .iter()
        .zip(nodes.iter())
        .map(|(row, n)| {
            let line_text = format_columns(
                [
                    row[0].as_str(),
                    row[1].as_str(),
                    row[2].as_str(),
                    row[3].as_str(),
                ],
                widths,
            );
            let style = if n.is_selectable() {
                Style::default()
            } else {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM)
            };
            ListItem::new(Line::from(Span::styled(line_text, style)))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, chunks[2], state);

    // Footer help.
    let hint = Paragraph::new(Line::from(vec![Span::styled(
        "[j/k] move   [Enter] select   [q/Esc] cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    frame.render_widget(hint, chunks[3]);
}

fn format_columns(cells: [&str; 4], widths: &[usize; 4]) -> String {
    // Two spaces between columns; final column not padded.
    format!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {}",
        cells[0],
        cells[1],
        cells[2],
        cells[3],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn node(
        pid: u32,
        config: &str,
        source: ConfigSource,
        status: ConfigStatus,
        port: Option<u16>,
        socket: Option<&str>,
    ) -> RunningNode {
        RunningNode {
            pid,
            config_path: PathBuf::from(config),
            config_source: source,
            port,
            socket: socket.map(PathBuf::from),
            status,
        }
    }

    #[test]
    fn format_row_explicit_ok_no_marker() {
        let n = node(
            12345,
            "/srv/preview/preview-config.json",
            ConfigSource::Explicit,
            ConfigStatus::Ok,
            Some(3001),
            Some("./node.sock"),
        );
        let r = format_row(&n);
        assert_eq!(r[0], "12345");
        assert_eq!(r[1], "3001");
        assert_eq!(r[2], "./node.sock");
        assert_eq!(r[3], "/srv/preview/preview-config.json");
    }

    #[test]
    fn format_row_default_appends_default_marker() {
        let n = node(
            7,
            "config/mainnet/config.json",
            ConfigSource::Default,
            ConfigStatus::Missing,
            None,
            None,
        );
        let r = format_row(&n);
        assert_eq!(r[1], "-");
        assert_eq!(r[2], "-");
        assert!(r[3].ends_with("(default) [missing]"));
    }

    #[test]
    fn format_row_permission_denied_marker() {
        let n = node(
            42,
            "rel.json",
            ConfigSource::Explicit,
            ConfigStatus::PermissionDenied,
            None,
            None,
        );
        let r = format_row(&n);
        assert!(r[3].ends_with("[permission denied]"));
    }

    #[test]
    fn column_widths_grow_to_widest_cell() {
        let rows = vec![
            [
                "1".to_string(),
                "3001".to_string(),
                "/tmp/a.sock".to_string(),
                "x".to_string(),
            ],
            [
                "999999".to_string(),
                "12345".to_string(),
                "longer-socket-path.sock".to_string(),
                "y".to_string(),
            ],
        ];
        let widths = column_widths(&rows);
        assert_eq!(widths[0], "999999".len());
        assert_eq!(widths[1], "12345".len());
        assert_eq!(widths[2], "longer-socket-path.sock".len());
        // Header "CONFIG" wins for the last column even though cells are shorter.
        assert_eq!(widths[3], "CONFIG".len());
    }

    #[test]
    fn move_cursor_wraps_at_boundaries() {
        let mut state = ListState::default();
        state.select(Some(0));
        move_cursor(&mut state, 3, -1);
        assert_eq!(state.selected(), Some(2));
        move_cursor(&mut state, 3, 1);
        assert_eq!(state.selected(), Some(0));
        move_cursor(&mut state, 3, 1);
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn move_cursor_no_op_on_empty_list() {
        let mut state = ListState::default();
        state.select(Some(0));
        move_cursor(&mut state, 0, 1);
        assert_eq!(state.selected(), Some(0));
    }
}
