use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use log::{debug, info, trace};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, InputMode};
use crate::executor::ExecutionResult;
use crate::task::{Task, TaskDetail, TaskStatus};

const TICK_RATE: Duration = Duration::from_millis(200);

pub fn run(app: &mut App) -> Result<()> {
    info!("Initializing TUI");
    trace!("Enabling raw mode");
    enable_raw_mode()
        .context("Failed to enable raw mode")?;
    
    trace!("Entering alternate screen");
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .context("Failed to enter alternate screen")?;
    
    trace!("Creating terminal backend");
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .context("Failed to create terminal")?;
    info!("Terminal initialized successfully");

    trace!("Starting main event loop");
    let res = run_loop(&mut terminal, app);
    
    trace!("Cleaning up TUI");
    disable_raw_mode()
        .context("Failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("Failed to leave alternate screen")?;
    terminal.show_cursor()
        .context("Failed to show cursor")?;
    info!("TUI cleanup completed");
    
    res
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    info!("Event loop started");
    let mut last_tick = Instant::now();
    let mut iteration_count = 0u64;
    
    loop {
        iteration_count += 1;
        // Only log iterations to file, not stderr (trace level)
        
        terminal.draw(|frame| draw(frame, app))
            .context("Failed to draw frame")?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if event::poll(timeout)
            .context("Failed to poll for events")? {
            match event::read()
                .context("Failed to read event")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.has_pending_approval() {
                        info!("Handling approval key");
                        match key.code {
                            KeyCode::Char('y') => {
                                info!("User approved blocked task");
                                app.approve_current_blocked();
                                continue;
                            }
                            KeyCode::Char('n') => {
                                info!("User rejected blocked task");
                                app.reject_current_blocked();
                                continue;
                            }
                            _ => {
                                log::debug!("Ignoring key during approval: {:?}", key.code);
                            }
                        }
                    }
                    let editing = matches!(app.input_mode, InputMode::Prompt);
                    match key.code {
                        KeyCode::Char('q') => {
                            info!("Quit key pressed, exiting event loop");
                            return Ok(());
                        }
                        KeyCode::Down | KeyCode::Char('j') if !editing => {
                            // If analysis is displayed, scroll it instead of moving selection
                            if app.analysis_result.is_some() {
                                log::trace!("Scrolling analysis down");
                                app.scroll_analysis_down();
                            } else {
                                log::trace!("Moving selection down");
                                app.move_next();
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') if !editing => {
                            // If analysis is displayed, scroll it instead of moving selection
                            if app.analysis_result.is_some() {
                                log::trace!("Scrolling analysis up");
                                app.scroll_analysis_up();
                            } else {
                                log::trace!("Moving selection up");
                                app.move_prev();
                            }
                        }
                        KeyCode::Tab => {
                            info!("Toggling input mode");
                            app.input_mode = match app.input_mode {
                                InputMode::Prompt => InputMode::Logs,
                                InputMode::Logs => InputMode::Prompt,
                            };
                        }
                        KeyCode::Enter if editing => {
                            // Shift+Enter or Ctrl+Enter adds newline, plain Enter submits
                            if key.modifiers.contains(KeyModifiers::SHIFT) || 
                               key.modifiers.contains(KeyModifiers::CONTROL) ||
                               key.modifiers.contains(KeyModifiers::ALT) {
                                log::trace!("Inserting newline (Shift/Ctrl/Alt+Enter)");
                                app.input.push('\n');
                            } else {
                                // Plain Enter submits if input is not empty
                                if !app.input.trim().is_empty() {
                                    app.submit_prompt();
                                } else {
                                    log::trace!("Enter pressed but input is empty, ignoring");
                                }
                            }
                        }
                        KeyCode::Backspace if editing => {
                            log::trace!("Backspace pressed");
                            if !app.input.is_empty() {
                                app.input.pop();
                            }
                        }
                        KeyCode::Char(c) if editing => {
                            app.input.push(c);
                        }
                        // 'r' key removed - sequential execution handles task flow automatically
                        KeyCode::Enter if !editing => {
                            // Enter when not editing runs the selected task
                            info!("User pressed Enter to run selected task");
                            app.execute_selected();
                        }
                        _ => {
                            log::trace!("Unhandled key: {:?}", key.code);
                        }
                    }
                }
                Event::Resize(width, height) => {
                    debug!("Terminal resized: {}x{}", width, height);
                }
                other => {
                    log::trace!("Other event: {:?}", other);
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            last_tick = Instant::now();
        }
        
        // Log every 1000 iterations to track if we're stuck (debug level, goes to file only)
        if iteration_count % 1000 == 0 {
            log::debug!("Event loop still running (iteration {})", iteration_count);
        }
    }
}

fn draw(frame: &mut Frame, app: &App) {
    // Calculate dynamic height for input area (up to 10 lines)
    let input_height = calculate_input_height(app, frame.size().width);
    
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(input_height),
            Constraint::Length(5),
        ])
        .split(frame.size());

    draw_header(frame, main_chunks[0], app);
    draw_body(frame, main_chunks[1], app);
    draw_input(frame, main_chunks[2], app);
    draw_logs(frame, main_chunks[3], app);
}

fn calculate_input_height(app: &App, available_width: u16) -> u16 {
    // Account for borders (left + right = 2) and title line (1)
    let border_width = 2;
    let title_height = 1;
    let usable_width = available_width.saturating_sub(border_width);
    
    if usable_width < 10 {
        return 3; // Minimum height (1 line + borders)
    }
    
    // Calculate how many lines the wrapped text would take
    let mut total_lines = 0;
    
    // Split by actual newlines first
    for line in app.input.lines() {
        if line.is_empty() {
            total_lines += 1;
        } else {
            // Calculate wrapping for this line
            let mut current_width = 0;
            total_lines += 1; // At least one line for this content
            
            for ch in line.chars() {
                // Approximate character width (ASCII = 1, others = 2)
                let char_width = if ch.is_ascii() { 1 } else { 2 };
                
                if current_width + char_width > usable_width as usize {
                    total_lines += 1;
                    current_width = char_width;
                } else {
                    current_width += char_width;
                }
            }
        }
    }
    
    // If input ends with newline, add one more line for cursor
    if app.input.ends_with('\n') {
        total_lines += 1;
    }
    
    // If empty, we still need at least one line
    if total_lines == 0 {
        total_lines = 1;
    }
    
    // Clamp between 3 (minimum: 1 line + borders) and 12 (max 10 content lines + borders + title)
    // Max content lines is 10, so max total height is 10 + 2 (borders) = 12
    let height = (total_lines + title_height).min(12).max(3);
    height as u16
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    // Count tasks by status
    let complete_count = app.tasks.iter().filter(|t| matches!(t.status, crate::task::TaskStatus::Complete)).count();
    let ready_count = app.tasks.iter().filter(|t| matches!(t.status, crate::task::TaskStatus::Ready)).count();
    let blocked_count = app.tasks.iter().filter(|t| matches!(t.status, crate::task::TaskStatus::Blocked(_))).count();
    let total_count = app.tasks.len();
    
    // Build status line
    let mut status_parts = Vec::new();
    if total_count > 0 {
        status_parts.push(format!("Total: {}", total_count));
        if complete_count > 0 {
            status_parts.push(format!("✓ {}", complete_count));
        }
        if ready_count > 0 {
            status_parts.push(format!("▶ {}", ready_count));
        }
        if blocked_count > 0 {
            status_parts.push(format!("⚠ {}", blocked_count));
        }
    }
    
    let status_line = if !status_parts.is_empty() {
        status_parts.join(" | ")
    } else {
        "Ready for your prompt".to_string()
    };
    
    // Show summary or analysis preview
    let content = if let Some(ref analysis) = app.analysis_result {
        // Show first few lines of analysis
        analysis.lines().take(2).collect::<Vec<_>>().join("\n")
    } else {
        app.summary
            .clone()
            .unwrap_or_else(|| "Request a plan to get started.".into())
    };
    
    let title = if app.analysis_result.is_some() {
        format!("Analysis ({})", status_line)
    } else if !app.tasks.is_empty() {
        format!("Plan ({})", status_line)
    } else {
        "SYSAIDMIN".to_string()
    };
    
    let header = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true })
        .style(if app.analysis_result.is_some() {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        });
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
            
            // Status icon
            let status_icon = match task.status {
                TaskStatus::Complete => "✓",
                TaskStatus::Ready => "▶",
                TaskStatus::Blocked(_) => "⚠",
                TaskStatus::Running => "⟳",
                TaskStatus::Proposed => "○",
            };
            
            // For Note tasks, show details if description is just "Note"
            let display_text = if matches!(task.detail, TaskDetail::Note { .. }) && task.description == "Note" {
                if let TaskDetail::Note { ref details } = task.detail {
                    // Truncate details to fit in list (max 60 chars)
                    if details.len() > 60 {
                        format!("{}…", &details[..60])
                    } else {
                        details.clone()
                    }
                } else {
                    task.description.clone()
                }
            } else {
                task.description.clone()
            };
            
            ListItem::new(Line::from(vec![
                Span::styled(indicator, style),
                Span::styled(status_icon, style),
                Span::raw(" "),
                Span::styled(display_text, style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Plan"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_widget(list, chunks[0]);

    // Split Details pane into top (details) and bottom (results)
    // Prioritize showing analysis result, then execution results
    let has_analysis = app.analysis_result.is_some();
    let has_execution_results = app.execution_results.contains_key(&app.selected);
    let has_results = has_analysis || has_execution_results;
    
    let constraints = if has_results {
        // Results exist: give details minimum space, results get the rest
        // Analysis gets more space than execution results
        if has_analysis {
            [Constraint::Min(3), Constraint::Min(10)] // More space for analysis
        } else {
            [Constraint::Min(5), Constraint::Min(5)] // Less space for execution results
        }
    } else {
        // No results: details get all space
        [Constraint::Min(0), Constraint::Length(0)]
    };
    
    let detail_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(chunks[1]);

    // Top: Task details (minimal when results exist)
    let detail_text = app
        .tasks
        .get(app.selected)
        .map(task_detail_lines)
        .unwrap_or_else(|| vec![Line::raw("No task selected")]);

    let detail = Paragraph::new(detail_text)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, detail_chunks[0]);

    // Bottom: Results pane - prioritize analysis, then execution results
        if has_results && detail_chunks[1].height > 2 {
            if has_analysis {
                // Show LLM analysis result with scrolling
                let analysis_lines: Vec<String> = app.analysis_result.as_ref()
                    .map(|analysis| analysis.lines().map(|s| s.to_string()).collect())
                    .unwrap_or_else(|| vec!["No analysis available".to_string()]);
                
                // Calculate available height (subtract borders and title)
                let available_height = detail_chunks[1].height.saturating_sub(2) as usize;
                
                // Clamp scroll offset to valid range
                let max_scroll = analysis_lines.len().saturating_sub(available_height);
                let scroll_offset = app.analysis_scroll_offset.min(max_scroll);
                
                // Get visible lines based on scroll offset
                let visible_lines: Vec<Line> = analysis_lines
                    .iter()
                    .skip(scroll_offset)
                    .take(available_height)
                    .map(|line| Line::raw(line.clone()))
                    .collect();

            let result = Paragraph::new(visible_lines)
                .block(Block::default().borders(Borders::ALL).title("Analysis (↑↓ to scroll)"))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::Green));
            frame.render_widget(result, detail_chunks[1]);
        } else if has_execution_results {
            // Show minimal execution results (exit code only, 1 line max)
            let result_text = app
                .execution_results
                .get(&app.selected)
                .map(format_execution_result_minimal)
                .unwrap_or_else(|| vec![Line::raw("No execution results")]);

            let result = Paragraph::new(result_text)
                .block(Block::default().borders(Borders::ALL).title("Results"))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Cyan));
            frame.render_widget(result, detail_chunks[1]);
        }
    }
}

#[allow(dead_code)] // Kept for potential future use
fn format_execution_result(result: &ExecutionResult) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Exit Code: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}", result.status)),
        ]),
    ];

    if !result.stdout.trim().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("STDOUT:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]));
        // Split stdout into lines, keeping long lines for wrapping
        for line in result.stdout.lines() {
            lines.push(Line::raw(line.to_string()));
        }
    }

    if !result.stderr.trim().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("STDERR:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Red)),
        ]));
        // Split stderr into lines
        for line in result.stderr.lines() {
            lines.push(Line::raw(line.to_string()));
        }
    }

    lines
}

/// Format execution result minimally (exit code only, 1 line max)
fn format_execution_result_minimal(result: &ExecutionResult) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled("Exit: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}", result.status)),
            Span::raw(" | "),
            Span::styled("Output: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(if !result.stdout.trim().is_empty() {
                "stdout"
            } else if !result.stderr.trim().is_empty() {
                "stderr"
            } else {
                "none"
            }),
        ]),
    ]
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(message) = app.pending_approval_message() {
        // Split message into lines and ensure it fits within available height
        let available_height = area.height.saturating_sub(2) as usize; // Subtract borders
        let message_lines: Vec<Line> = message
            .lines()
            .take(available_height)
            .map(|line| {
                // Truncate each line to fit available width
                let available_width = area.width.saturating_sub(2) as usize;
                let truncated = if line.len() > available_width {
                    let mut truncated = line.chars().take(available_width.saturating_sub(1)).collect::<String>();
                    truncated.push('…');
                    truncated
                } else {
                    line.to_string()
                };
                Line::raw(truncated)
            })
            .collect();
        
        let block = Paragraph::new(message_lines)
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Approval required (y = allow, n = skip)"),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(block, area);
        return;
    }

    let title = match app.input_mode {
        InputMode::Prompt => "Prompt (Enter=submit, Shift+Enter=newline, q=quit)",
        InputMode::Logs => "Prompt (logs focused - press Tab to edit)",
    };
    
    // Use the input string directly - Paragraph will handle wrapping automatically
    // and respect explicit newlines
    let input_text = if app.input.is_empty() {
        " " // Show at least a space so the area is visible
    } else {
        app.input.as_str()
    };
    
    let input = Paragraph::new(input_text)
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, area);
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    // Calculate available width (subtract borders: 2 chars)
    let available_width = area.width.saturating_sub(2) as usize;
    
    let logs: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(4)
        .map(|entry| {
            // Truncate each log entry to fit available width
            let truncated = if entry.len() > available_width {
                let mut truncated = entry.chars().take(available_width.saturating_sub(3)).collect::<String>();
                truncated.push_str("…");
                truncated
            } else {
                entry.clone()
            };
            Line::raw(truncated)
        })
        .collect();
    
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
