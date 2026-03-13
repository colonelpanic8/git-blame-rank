use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};

use crate::app::{AppState, FocusPane, NodeKind, RecentFileStatus};

pub fn run(
    app_state: &mut AppState,
    event_rx: &crossbeam_channel::Receiver<crate::event::WorkerEvent>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, app_state, event_rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app_state: &mut AppState,
    event_rx: &crossbeam_channel::Receiver<crate::event::WorkerEvent>,
) -> anyhow::Result<()> {
    loop {
        while let Ok(worker_event) = event_rx.try_recv() {
            app_state.apply_worker_event(worker_event);
        }

        terminal.draw(|frame| draw(frame, app_state))?;

        if event::poll(Duration::from_millis(75))? {
            if let CrosstermEvent::Key(key_event) = event::read()?
                && key_event.kind == KeyEventKind::Press
            {
                match key_event.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Tab => app_state.cycle_focus(),
                    KeyCode::Down | KeyCode::Char('j') => match app_state.focus {
                        FocusPane::Tree => app_state.move_tree_selection(1),
                        FocusPane::Extensions => app_state.move_extension_selection(1),
                    },
                    KeyCode::Up | KeyCode::Char('k') => match app_state.focus {
                        FocusPane::Tree => app_state.move_tree_selection(-1),
                        FocusPane::Extensions => app_state.move_extension_selection(-1),
                    },
                    KeyCode::Left | KeyCode::Char('h') => {
                        if app_state.focus == FocusPane::Tree {
                            app_state.collapse_or_select_parent();
                        }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        if app_state.focus == FocusPane::Tree {
                            app_state.expand_selected();
                        }
                    }
                    KeyCode::Char(' ') => match app_state.focus {
                        FocusPane::Tree => app_state.toggle_selected_tree_node(),
                        FocusPane::Extensions => app_state.toggle_selected_extension(),
                    },
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, app_state: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(14),
            Constraint::Length(8),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(layout[1]);

    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(8)])
        .split(main_chunks[0]);

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(8)])
        .split(main_chunks[1]);

    let (scope_done, scope_total) = app_state.current_scope_file_counts();
    let elapsed = format_duration(app_state.elapsed());
    let scan_state = if app_state.is_finished() {
        "complete"
    } else {
        "running"
    };
    let summary = Paragraph::new(format!(
        "repo: {}\nstate: {}  elapsed: {}  rev: {}\nscope: {}  focus: {}  jobs: {}  running: {}\ndone: {}/{}  scope files: {}/{}  failures: {}  lines: {}",
        app_state.repo_root.display(),
        scan_state,
        elapsed,
        app_state.rev,
        app_state.current_scope_label(),
        focus_label(app_state.focus),
        app_state.jobs,
        app_state.running_files(),
        app_state.processed_files,
        app_state.total_files,
        scope_done,
        scope_total,
        app_state.failed_files,
        app_state.total_lines,
    ))
    .block(Block::default().title("Scan").borders(Borders::ALL));
    frame.render_widget(summary, layout[0]);

    draw_tree(frame, app_state, left_chunks[0]);
    draw_extensions(frame, app_state, left_chunks[1]);
    draw_rankings(frame, app_state, right_chunks[0]);
    draw_recent(frame, app_state, right_chunks[1]);

    let scan_state = if app_state.is_finished() {
        "scan complete"
    } else {
        "scanning..."
    };
    let footer_text = format!(
        "{scan_state}  focus={}  tab switch pane  q/esc quit\n\
tree: j/k or arrows move  h/l or arrows collapse expand  space toggle subtree/file\n\
ext:  j/k or arrows move  space toggle extension",
        focus_label(app_state.focus),
    );
    let footer =
        Paragraph::new(footer_text).block(Block::default().title("Bindings").borders(Borders::ALL));
    frame.render_widget(footer, layout[3]);
}

fn draw_tree(frame: &mut Frame<'_>, app_state: &AppState, area: ratatui::layout::Rect) {
    let visible = app_state.visible_tree_nodes();
    let selected_index = visible
        .iter()
        .position(|node| node.node_id == app_state.selected_tree_node)
        .unwrap_or(0);
    let window = visible_window(
        selected_index,
        visible.len(),
        area.height.saturating_sub(3) as usize,
    );

    let rows = visible[window.clone()]
        .iter()
        .enumerate()
        .map(|(offset, visible_node)| {
            let node = &app_state.tree_nodes[visible_node.node_id];
            let marker = if node.enabled { "[x]" } else { "[ ]" };
            let expander = match node.kind {
                NodeKind::File => " ",
                NodeKind::Directory if node.expanded => "▾",
                NodeKind::Directory => "▸",
            };
            let indent = "  ".repeat(visible_node.depth);
            let label = format!(
                "{indent}{marker} {expander} {}",
                if visible_node.node_id == 0 {
                    ".".to_owned()
                } else {
                    node.name.to_string()
                }
            );
            let row = Row::new([
                Cell::from(label),
                Cell::from(format!("{}/{}", node.processed_files, node.total_files)),
            ]);

            if window.start + offset == selected_index && app_state.focus == FocusPane::Tree {
                row.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });

    let table = Table::new(rows, [Constraint::Min(10), Constraint::Length(10)])
        .header(Row::new(["path", "done"]).style(Style::default().add_modifier(Modifier::BOLD)))
        .block(Block::default().title("Tree").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn draw_extensions(frame: &mut Frame<'_>, app_state: &AppState, area: ratatui::layout::Rect) {
    let selected_index = app_state
        .selected_extension
        .min(app_state.extension_filters.len().saturating_sub(1));
    let window = visible_window(
        selected_index,
        app_state.extension_filters.len(),
        area.height.saturating_sub(3) as usize,
    );

    let rows = app_state.extension_filters[window.clone()]
        .iter()
        .enumerate()
        .map(|(offset, filter)| {
            let row = Row::new([
                Cell::from(if filter.enabled { "[x]" } else { "[ ]" }),
                Cell::from(filter.extension.to_string()),
                Cell::from(format!("{}/{}", filter.processed_files, filter.total_files)),
            ]);

            if window.start + offset == selected_index && app_state.focus == FocusPane::Extensions {
                row.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });

    let title = format!("Extensions ({})", app_state.selected_extension_label());
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(10),
        ],
    )
    .header(Row::new(["on", "ext", "done"]).style(Style::default().add_modifier(Modifier::BOLD)))
    .block(Block::default().title(title).borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn draw_rankings(frame: &mut Frame<'_>, app_state: &AppState, area: ratatui::layout::Rect) {
    let author_rows = app_state
        .author_rows()
        .into_iter()
        .take(area.height.saturating_sub(3) as usize)
        .map(|row| {
            Row::new([
                Cell::from(row.author.display_name().to_owned()),
                Cell::from(row.author.email.to_string()),
                Cell::from(row.lines.to_string()),
                Cell::from(row.files.to_string()),
                Cell::from(row.commits.to_string()),
            ])
        });

    let title = format!("Rankings ({})", app_state.current_scope_label());
    let author_table = Table::new(
        author_rows,
        [
            Constraint::Length(20),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(["author", "email", "lines", "files", "commits"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title(title).borders(Borders::ALL));
    frame.render_widget(author_table, area);
}

fn draw_recent(frame: &mut Frame<'_>, app_state: &AppState, area: ratatui::layout::Rect) {
    let recent_rows = app_state
        .recent_files
        .iter()
        .take(area.height.saturating_sub(3) as usize)
        .map(|file| {
            let status = match file.status {
                RecentFileStatus::Complete => "ok",
                RecentFileStatus::Failed => "err",
            };

            Row::new([
                Cell::from(status),
                Cell::from(file.lines.to_string()),
                Cell::from(format_duration(file.elapsed)),
                Cell::from(file.path.clone()),
            ])
        });
    let recent_table = Table::new(
        recent_rows,
        [
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(["state", "lines", "elapsed", "path"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title("Recent Files").borders(Borders::ALL));
    frame.render_widget(recent_table, area);
}

fn focus_label(focus: FocusPane) -> &'static str {
    match focus {
        FocusPane::Tree => "tree",
        FocusPane::Extensions => "extensions",
    }
}

fn visible_window(selected: usize, total: usize, height: usize) -> std::ops::Range<usize> {
    if total == 0 || height == 0 {
        return 0..0;
    }

    if total <= height {
        return 0..total;
    }

    let half = height / 2;
    let mut start = selected.saturating_sub(half);
    let mut end = start + height;
    if end > total {
        end = total;
        start = end.saturating_sub(height);
    }
    start..end
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let millis = duration.subsec_millis();

    if seconds < 60 {
        return format!("{seconds}.{millis:03}s");
    }

    let minutes = seconds / 60;
    let remaining_seconds = seconds % 60;

    if minutes < 60 {
        return format!("{minutes}:{remaining_seconds:02}.{millis:03}");
    }

    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    format!("{hours}:{remaining_minutes:02}:{remaining_seconds:02}.{millis:03}")
}
