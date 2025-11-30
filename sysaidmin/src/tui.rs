use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use log::{debug, info, trace};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;

const TICK_RATE: Duration = Duration::from_millis(50);
const CURSOR_BLINK_RATE: Duration = Duration::from_millis(500);

/// Message types for the message stream
#[derive(Debug, Clone)]
pub enum MessageType {
    Info,
    Command,
    Success,
    Warning,
    Error,
    Recommendation,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub content: String,
    pub msg_type: MessageType,
}

pub fn run(app: &mut App) -> Result<()> {
    info!("Initializing TUI");
    
    // Clear the screen before starting
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All)).context("Failed to clear screen")?;
    
    trace!("Enabling raw mode");
    enable_raw_mode().context("Failed to enable raw mode")?;

    trace!("Creating terminal backend");
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;
    info!("Terminal initialized successfully");

    // Add initial usage messages
    app.add_message(
        "Welcome to sysaidmin - Production Server Debugging Assistant".to_string(),
        MessageType::Info,
    );
    app.add_message(
        "Type your prompt below and press Enter. Use Ctrl+C or 'q' to quit.".to_string(),
        MessageType::Info,
    );
    app.add_message(
        "All commands will be logged to sysaidmin.history.sh".to_string(),
        MessageType::Info,
    );

    trace!("Starting main event loop");
    let res = run_loop(&mut terminal, app);

    trace!("Cleaning up TUI");
    disable_raw_mode().context("Failed to disable raw mode")?;
    terminal.show_cursor().context("Failed to show cursor")?;
    info!("TUI cleanup completed");

    res
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    info!("Event loop started");
    let mut last_tick = Instant::now();
    let mut last_cursor_blink = Instant::now();
    let mut cursor_visible = true;

    loop {
        // Check for asynchronous plan responses before drawing
        app.poll_plan_response();

        // Update cursor blink
        if last_cursor_blink.elapsed() >= CURSOR_BLINK_RATE {
            cursor_visible = !cursor_visible;
            last_cursor_blink = Instant::now();
        }

        terminal
            .draw(|frame| draw(frame, app, cursor_visible))
            .context("Failed to draw frame")?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if event::poll(timeout).context("Failed to poll for events")? {
            match event::read().context("Failed to read event")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            info!("Quit key pressed, exiting event loop");
                            return Ok(());
                        }
                        KeyCode::Enter => {
                            let prompt = app.input.trim().to_string();
                            if !prompt.is_empty() {
                                app.submit_prompt();
                            }
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        KeyCode::Up => {
                            // Scroll up in message history
                            app.scroll_messages_up();
                        }
                        KeyCode::Down => {
                            // Scroll down in message history
                            app.scroll_messages_down();
                        }
                        KeyCode::PageUp => {
                            // Scroll up by page
                            for _ in 0..10 {
                                app.scroll_messages_up();
                            }
                        }
                        KeyCode::PageDown => {
                            // Scroll down by page
                            for _ in 0..10 {
                                app.scroll_messages_down();
                            }
                        }
                        _ => {
                            trace!("Unhandled key: {:?}", key.code);
                        }
                    }
                }
                Event::Resize(width, height) => {
                    debug!("Terminal resized: {}x{}", width, height);
                }
                other => {
                    trace!("Other event: {:?}", other);
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            last_tick = Instant::now();
        }
    }
}

fn draw(frame: &mut Frame, app: &App, cursor_visible: bool) {
    // Calculate prompt height dynamically based on input content
    let prompt_height = calculate_prompt_height(app, frame.size().width);
    
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),  // Message stream (takes remaining space)
            Constraint::Length(prompt_height), // Prompt area (dynamic)
        ])
        .split(frame.size());

    draw_message_stream(frame, chunks[0], app);
    draw_prompt(frame, chunks[1], app, cursor_visible);
}

fn calculate_prompt_height(app: &App, available_width: u16) -> u16 {
    // Account for borders (left + right = 2) and prompt prefix "> "
    let border_width = 2;
    let prefix_width = 2; // "> "
    let usable_width = available_width.saturating_sub(border_width + prefix_width);

    if usable_width < 10 {
        return 3; // Minimum height
    }

    // Calculate how many lines the wrapped text would take
    let mut total_lines = 1; // At least one line

    // Split by actual newlines first
    for line in app.input.lines() {
        if line.is_empty() {
            total_lines += 1;
        } else {
            // Calculate wrapping for this line
            let mut current_width = 0usize;

            for ch in line.chars() {
                // Approximate character width (ASCII = 1, others = 2)
                let char_width = if ch.is_ascii() { 1 } else { 2 };

                if current_width + char_width > usable_width as usize && current_width > 0 {
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

    // Add 2 for borders (top + bottom)
    let height = (total_lines + 2).min(20).max(3); // Max 20 lines, min 3
    height as u16
}

fn draw_message_stream(frame: &mut Frame, area: Rect, app: &App) {
    // Get all messages and convert to lines with proper wrapping
    let available_width = area.width as usize;
    
    let mut all_lines: Vec<Line> = Vec::new();
    
    // Process messages in order (oldest first)
    for msg in app.get_all_messages().iter() {
        let style = message_style(&msg.msg_type);
        let prefix = message_prefix(&msg.msg_type);
        let prefix_width = prefix.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum::<usize>();
        let content_width = available_width.saturating_sub(prefix_width);
        
        // Split message into lines and wrap each line
        for line in msg.content.lines() {
            let wrapped = wrap_text(line, content_width.max(1));
            for wrapped_line in wrapped {
                all_lines.push(Line::from(vec![
                    Span::styled(prefix.clone(), style),
                    Span::styled(wrapped_line, style),
                ]));
            }
        }
    }

    // Show only the last N lines that fit in the area
    // If we have fewer lines than the area height, pad with empty lines at the top
    // so messages appear at the bottom (natural flow)
    let max_lines = area.height as usize;
    let mut visible_lines: Vec<Line> = Vec::new();
    
    if all_lines.len() > max_lines {
        // Show only the most recent lines
        let start_idx = all_lines.len() - max_lines;
        visible_lines = all_lines.iter().skip(start_idx).cloned().collect();
    } else {
        // Pad with empty lines at the top so messages appear at bottom
        let empty_lines = max_lines - all_lines.len();
        for _ in 0..empty_lines {
            visible_lines.push(Line::raw(""));
        }
        visible_lines.extend_from_slice(&all_lines);
    }

    let paragraph = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    
    let mut result = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;
    
    for ch in text.chars() {
        let char_width = if ch.is_ascii() { 1 } else { 2 };
        
            if ch == '\n' {
                if !current_line.is_empty() {
                    result.push(current_line.clone());
                    current_line.clear();
                }
                result.push(String::new());
                current_width = 0;
            } else if current_width + char_width > max_width && !current_line.is_empty() {
                result.push(current_line.clone());
                current_line.clear();
                current_line.push(ch);
                current_width = char_width;
        } else {
            current_line.push(ch);
            current_width += char_width;
        }
    }
    
    if !current_line.is_empty() {
        result.push(current_line);
    }
    
    if result.is_empty() {
        result.push(String::new());
    }
    
    result
}

fn draw_prompt(frame: &mut Frame, area: Rect, app: &App, cursor_visible: bool) {
    // Build prompt text with cursor
    let prompt_lines: Vec<Line> = if app.input.is_empty() {
        vec![Line::from(vec![
            Span::raw("> "),
            Span::styled(if cursor_visible { "_" } else { " " }, Style::default().fg(Color::Cyan)),
        ])]
    } else {
        app.input
            .lines()
            .enumerate()
            .map(|(idx, line)| {
                let is_last = idx == app.input.lines().count() - 1;
                if is_last && cursor_visible {
                    Line::from(vec![
                        Span::raw("> "),
                        Span::styled(line.to_string(), Style::default().fg(Color::Cyan)),
                        Span::styled("_", Style::default().fg(Color::Cyan)),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("> "),
                        Span::styled(line.to_string(), Style::default().fg(Color::Cyan)),
                    ])
                }
            })
            .collect()
    };

    let paragraph = Paragraph::new(prompt_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

fn message_style(msg_type: &MessageType) -> Style {
    match msg_type {
        MessageType::Info => Style::default().fg(Color::White),
        MessageType::Command => Style::default().fg(Color::Yellow),
        MessageType::Success => Style::default().fg(Color::Green),
        MessageType::Warning => Style::default().fg(Color::Yellow),
        MessageType::Error => Style::default().fg(Color::Red),
        MessageType::Recommendation => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    }
}

fn message_prefix(msg_type: &MessageType) -> String {
    match msg_type {
        MessageType::Info => "".to_string(),
        MessageType::Command => "→ ".to_string(),
        MessageType::Success => "✓ ".to_string(),
        MessageType::Warning => "⚠ ".to_string(),
        MessageType::Error => "✗ ".to_string(),
        MessageType::Recommendation => "💡 ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_prefix_formats_correctly() {
        assert_eq!(message_prefix(&MessageType::Command), "→ ");
        assert_eq!(message_prefix(&MessageType::Success), "✓ ");
        assert_eq!(message_prefix(&MessageType::Error), "✗ ");
    }

    #[test]
    fn message_style_returns_style() {
        let style = message_style(&MessageType::Info);
        assert_eq!(style.fg, Some(Color::White));
    }
}
