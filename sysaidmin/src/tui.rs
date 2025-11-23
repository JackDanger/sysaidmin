use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, InputMode};
use crate::task::{Task, TaskDetail, TaskStatus};

const TICK_RATE: Duration = Duration::from_millis(200);

pub fn run(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|frame| draw(frame, app))?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.has_pending_approval() {
                        match key.code {
                            KeyCode::Char('y') => {
                                app.approve_current_blocked();
                                continue;
                            }
                            KeyCode::Char('n') => {
                                app.reject_current_blocked();
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let editing = matches!(app.input_mode, InputMode::Prompt);
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') if !editing => app.move_next(),
                        KeyCode::Up | KeyCode::Char('k') if !editing => app.move_prev(),
                        KeyCode::Tab => {
                            app.input_mode = match app.input_mode {
                                InputMode::Prompt => InputMode::Logs,
                                InputMode::Logs => InputMode::Prompt,
                            }
                        }
                        KeyCode::Enter if editing => {
                            app.submit_prompt();
                        }
                        KeyCode::Backspace if editing => {
                            app.input.pop();
                        }
                        KeyCode::Char(c) if editing => {
                            app.input.push(c);
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            last_tick = Instant::now();
        }
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
            Constraint::Length(5),
        ])
        .split(frame.size());

    draw_header(frame, main_chunks[0], app);
    draw_body(frame, main_chunks[1], app);
    draw_input(frame, main_chunks[2], app);
    draw_logs(frame, main_chunks[3], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let summary = app
        .summary
        .clone()
        .unwrap_or_else(|| "Request a plan to get started.".into());
    let title = if app.dry_run() {
        "Summary [DRY-RUN]"
    } else {
        "Summary"
    };
    let header = Paragraph::new(summary)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });
    frame.render_widget(header, area);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .enumerate()
        .map(|(idx, task)| {
            let style = status_style(&task.status);
            let indicator = if idx == app.selected { "> " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(indicator, style),
                Span::styled(task.description.clone(), style),
                Span::raw(" "),
                Span::styled(format!("[{}]", task.status.label()), style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Plan"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_widget(list, chunks[0]);

    let detail_text = app
        .tasks
        .get(app.selected)
        .map(task_detail_lines)
        .unwrap_or_else(|| vec![Line::raw("No task selected")]);

    let detail = Paragraph::new(detail_text)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, chunks[1]);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(message) = app.pending_approval_message() {
        let block = Paragraph::new(message)
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Approval required (y = allow, n = skip)"),
            );
        frame.render_widget(block, area);
        return;
    }

    let title = match app.input_mode {
        InputMode::Prompt => "Prompt (Enter=plan, q=quit)",
        InputMode::Logs => "Prompt (logs focused - press Tab to edit)",
    };
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(input, area);
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let logs = app
        .logs
        .iter()
        .rev()
        .take(4)
        .map(|entry| Line::raw(entry.clone()))
        .collect::<Vec<_>>();
    let log_widget = Paragraph::new(logs)
        .block(Block::default().borders(Borders::ALL).title("Logs"))
        .wrap(Wrap { trim: true });
    frame.render_widget(log_widget, area);
}

fn status_style(status: &TaskStatus) -> Style {
    match status {
        TaskStatus::Proposed => Style::default().fg(Color::Yellow),
        TaskStatus::Ready => Style::default().fg(Color::Green),
        TaskStatus::Blocked(_) => Style::default().fg(Color::Red),
        TaskStatus::Running => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        TaskStatus::Complete => Style::default().fg(Color::Gray),
    }
}

fn task_detail_lines(task: &Task) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Description: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(task.description.clone()),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(task.status_text()),
        ]),
    ];

    match &task.detail {
        TaskDetail::Command(cmd) => {
            lines.push(Line::from(vec![
                Span::styled("Shell: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(cmd.shell.clone()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Command: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(cmd.command.clone()),
            ]));
            if let Some(cwd) = &cmd.cwd {
                lines.push(Line::from(vec![
                    Span::styled("CWD: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(cwd.clone()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled(
                    "Requires root: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{}", cmd.requires_root)),
            ]));
        }
        TaskDetail::FileEdit(edit) => {
            if let Some(path) = &edit.path {
                lines.push(Line::from(vec![
                    Span::styled("Path: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(path.clone()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("Length: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} bytes", edit.new_text.len())),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Preview: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(edit.new_text.chars().take(120).collect::<String>()),
            ]));
        }
        TaskDetail::Note { details } => {
            lines.push(Line::from(vec![
                Span::styled("Note: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(details.clone()),
            ]));
        }
    }

    lines
}
