//! Terminal UI Dashboard for observability
//!
//! Provides a real-time view of:
//! - Running agents and their status
//! - Live event stream
//! - Token usage and cost tracking
//! - Log file tailing

use super::events::{EventKind, ObservabilityEvent};
use super::{latest_log_file, read_events, AccumulatedUsage};
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Dashboard state
pub struct Dashboard {
    /// Log directory to watch
    log_dir: PathBuf,
    /// Current log file
    current_log: Option<PathBuf>,
    /// Loaded events
    events: Vec<ObservabilityEvent>,
    /// Accumulated usage
    usage: AccumulatedUsage,
    /// Scroll offset for events list
    scroll_offset: usize,
    /// Whether to auto-scroll
    auto_scroll: bool,
    /// Last refresh time
    last_refresh: Instant,
    /// Refresh interval
    refresh_interval: Duration,
    /// Selected tab (0 = events, 1 = usage, 2 = help)
    selected_tab: usize,
}

impl Dashboard {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            log_dir,
            current_log: None,
            events: Vec::new(),
            usage: AccumulatedUsage::default(),
            scroll_offset: 0,
            auto_scroll: true,
            last_refresh: Instant::now(),
            refresh_interval: Duration::from_secs(1),
            selected_tab: 0,
        }
    }

    /// Run the dashboard TUI
    pub fn run(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Initial load
        self.refresh()?;

        // Main loop
        let result = self.run_loop(&mut terminal);

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    fn run_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // Draw UI
            terminal.draw(|frame| self.draw(frame))?;

            // Handle input (with timeout for refresh)
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('r') => self.refresh()?,
                            KeyCode::Char('a') => self.auto_scroll = !self.auto_scroll,
                            KeyCode::Up | KeyCode::Char('k') => {
                                self.auto_scroll = false;
                                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                self.auto_scroll = false;
                                if self.scroll_offset < self.events.len().saturating_sub(1) {
                                    self.scroll_offset += 1;
                                }
                            }
                            KeyCode::PageUp => {
                                self.auto_scroll = false;
                                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                            }
                            KeyCode::PageDown => {
                                self.auto_scroll = false;
                                self.scroll_offset = (self.scroll_offset + 10)
                                    .min(self.events.len().saturating_sub(1));
                            }
                            KeyCode::Home => {
                                self.auto_scroll = false;
                                self.scroll_offset = 0;
                            }
                            KeyCode::End => {
                                self.auto_scroll = true;
                                self.scroll_offset = self.events.len().saturating_sub(1);
                            }
                            KeyCode::Tab => {
                                self.selected_tab = (self.selected_tab + 1) % 3;
                            }
                            KeyCode::Char('1') => self.selected_tab = 0,
                            KeyCode::Char('2') => self.selected_tab = 1,
                            KeyCode::Char('3') => self.selected_tab = 2,
                            _ => {}
                        }
                    }
                }
            }

            // Auto-refresh
            if self.last_refresh.elapsed() >= self.refresh_interval {
                self.refresh()?;
            }
        }
    }

    fn refresh(&mut self) -> Result<()> {
        // Find latest log file
        if let Ok(Some(log_file)) = latest_log_file(&self.log_dir) {
            // Only reload if file changed
            if self.current_log.as_ref() != Some(&log_file) {
                self.current_log = Some(log_file.clone());
                self.events.clear();
                self.usage = AccumulatedUsage::default();
            }

            // Read events
            if let Ok(events) = read_events(&log_file) {
                self.events = events;

                // Calculate usage from events
                self.usage = AccumulatedUsage::default();
                for event in &self.events {
                    if let EventKind::Usage {
                        input_tokens,
                        output_tokens,
                        cost_usd,
                        ..
                    } = &event.event
                    {
                        self.usage.total_input_tokens += input_tokens;
                        self.usage.total_output_tokens += output_tokens;
                        self.usage.request_count += 1;
                        if let Some(cost) = cost_usd {
                            self.usage.estimated_cost_usd += cost;
                        }
                    }
                }

                // Auto-scroll to bottom
                if self.auto_scroll && !self.events.is_empty() {
                    self.scroll_offset = self.events.len().saturating_sub(1);
                }
            }
        }

        self.last_refresh = Instant::now();
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();

        // Layout: header, main content, footer
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(0),    // Main content
                Constraint::Length(3), // Footer
            ])
            .split(area);

        // Header
        self.draw_header(frame, chunks[0]);

        // Main content based on selected tab
        match self.selected_tab {
            0 => self.draw_events(frame, chunks[1]),
            1 => self.draw_usage(frame, chunks[1]),
            2 => self.draw_help(frame, chunks[1]),
            _ => {}
        }

        // Footer
        self.draw_footer(frame, chunks[2]);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let tabs = vec![
            if self.selected_tab == 0 {
                "[1] Events"
            } else {
                " 1  Events"
            },
            if self.selected_tab == 1 {
                "[2] Usage"
            } else {
                " 2  Usage"
            },
            if self.selected_tab == 2 {
                "[3] Help"
            } else {
                " 3  Help"
            },
        ];

        let header_text = format!(
            "gsh Dashboard  |  {}  |  {}  |  {}  |  Events: {}",
            tabs[0],
            tabs[1],
            tabs[2],
            self.events.len()
        );

        let header = Paragraph::new(header_text)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );

        frame.render_widget(header, area);
    }

    fn draw_events(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .events
            .iter()
            .enumerate()
            .skip(self.scroll_offset.saturating_sub(area.height as usize / 2))
            .take(area.height as usize)
            .map(|(i, event)| {
                let (symbol, color, text) = format_event(event);
                let line = Line::from(vec![
                    Span::styled(
                        format!("{:4} ", i + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        event.ts.format("%H:%M:%S ").to_string(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{} ", symbol), Style::default().fg(color)),
                    Span::styled(
                        format!("[{}] ", event.agent),
                        Style::default().fg(Color::Blue),
                    ),
                    Span::raw(text),
                ]);
                ListItem::new(line)
            })
            .collect();

        let auto_indicator = if self.auto_scroll { " [AUTO]" } else { "" };
        let list = List::new(items).block(
            Block::default()
                .title(format!("Events{}", auto_indicator))
                .borders(Borders::ALL),
        );

        frame.render_widget(list, area);
    }

    fn draw_usage(&self, frame: &mut Frame, area: Rect) {
        let usage_text = vec![
            Line::from(vec![
                Span::raw("Input Tokens:  "),
                Span::styled(
                    format!("{:>12}", self.usage.total_input_tokens),
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from(vec![
                Span::raw("Output Tokens: "),
                Span::styled(
                    format!("{:>12}", self.usage.total_output_tokens),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(vec![
                Span::raw("Total Tokens:  "),
                Span::styled(
                    format!(
                        "{:>12}",
                        self.usage.total_input_tokens + self.usage.total_output_tokens
                    ),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Requests:      "),
                Span::styled(
                    format!("{:>12}", self.usage.request_count),
                    Style::default().fg(Color::Magenta),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Est. Cost:     "),
                Span::styled(
                    format!("${:>11.4}", self.usage.estimated_cost_usd),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(""),
            Line::from(Span::styled(
                "Per-Model Breakdown:",
                Style::default().add_modifier(Modifier::UNDERLINED),
            )),
        ];

        let mut lines = usage_text;

        // Add per-model breakdown
        for (model, usage) in &self.usage.by_model {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(format!("  {}", model), Style::default().fg(Color::Blue)),
            ]));
            lines.push(Line::from(vec![
                Span::raw("    Tokens: "),
                Span::raw(format!(
                    "{}in / {}out",
                    usage.input_tokens, usage.output_tokens
                )),
            ]));
            lines.push(Line::from(vec![
                Span::raw("    Cost:   "),
                Span::styled(
                    format!("${:.4}", usage.estimated_cost_usd),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }

        let usage_widget = Paragraph::new(lines)
            .block(Block::default().title("Token Usage").borders(Borders::ALL))
            .wrap(Wrap { trim: false });

        frame.render_widget(usage_widget, area);
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let help_text = vec![
            Line::from(Span::styled(
                "Keyboard Shortcuts",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED),
            )),
            Line::from(""),
            Line::from("  q, Esc       Quit dashboard"),
            Line::from("  Tab          Switch tabs"),
            Line::from("  1, 2, 3      Jump to tab"),
            Line::from(""),
            Line::from(Span::styled(
                "Events Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  j, Down      Scroll down"),
            Line::from("  k, Up        Scroll up"),
            Line::from("  PgDn/PgUp    Scroll by page"),
            Line::from("  Home         Jump to start"),
            Line::from("  End          Jump to end"),
            Line::from("  a            Toggle auto-scroll"),
            Line::from("  r            Force refresh"),
            Line::from(""),
            Line::from(Span::styled(
                "Event Symbols",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  >  Prompt received"),
            Line::from("  +  Agent spawned"),
            Line::from("  *  Agent started"),
            Line::from("  T  Tool called"),
            Line::from("  =  Tool result"),
            Line::from("  #  Text output"),
            Line::from("  !  Completion"),
            Line::from("  X  Error"),
            Line::from("  $  Token usage"),
        ];

        let help = Paragraph::new(help_text)
            .block(Block::default().title("Help").borders(Borders::ALL))
            .wrap(Wrap { trim: false });

        frame.render_widget(help, area);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let log_file = self
            .current_log
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "No log file".to_string());

        let footer_text = format!(
            "Log: {} | Last refresh: {}s ago | Press 'q' to quit, '?' for help",
            log_file,
            self.last_refresh.elapsed().as_secs()
        );

        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));

        frame.render_widget(footer, area);
    }
}

/// Format an event for display
fn format_event(event: &ObservabilityEvent) -> (&'static str, Color, String) {
    match &event.event {
        EventKind::Prompt { content } => {
            let truncated = if content.len() > 60 {
                format!("{}...", &content[..57])
            } else {
                content.clone()
            };
            (">", Color::Cyan, format!("Prompt: {}", truncated))
        }
        EventKind::Spawn { role, model, .. } => {
            let model_info = model
                .as_ref()
                .map(|m| format!(" ({})", m))
                .unwrap_or_default();
            ("+", Color::Green, format!("Spawn: {}{}", role, model_info))
        }
        EventKind::Start { flow } => {
            let flow_info = flow
                .as_ref()
                .map(|f| format!(" flow={}", f))
                .unwrap_or_default();
            ("*", Color::Blue, format!("Start{}", flow_info))
        }
        EventKind::Text { content } => {
            let truncated = if content.len() > 50 {
                format!("{}...", &content[..47])
            } else {
                content.clone()
            };
            ("#", Color::White, truncated.replace('\n', " "))
        }
        EventKind::ToolCall { tool, .. } => ("T", Color::Yellow, format!("Call: {}", tool)),
        EventKind::ToolResult {
            tool,
            success,
            duration_ms,
            ..
        } => {
            let status = if *success { "OK" } else { "FAIL" };
            let duration = duration_ms
                .map(|d| format!(" ({}ms)", d))
                .unwrap_or_default();
            (
                "=",
                if *success { Color::Green } else { Color::Red },
                format!("Result: {} {} {}", tool, status, duration),
            )
        }
        EventKind::Complete { next, .. } => {
            let next_info = next
                .as_ref()
                .map(|n| format!(" -> {}", n))
                .unwrap_or_else(|| " (end)".to_string());
            ("!", Color::Magenta, format!("Complete{}", next_info))
        }
        EventKind::Error { message, .. } => {
            let truncated = if message.len() > 50 {
                format!("{}...", &message[..47])
            } else {
                message.clone()
            };
            ("X", Color::Red, format!("Error: {}", truncated))
        }
        EventKind::Usage {
            input_tokens,
            output_tokens,
            cost_usd,
            ..
        } => {
            let cost = cost_usd
                .map(|c| format!(" ${:.4}", c))
                .unwrap_or_default();
            (
                "$",
                Color::Yellow,
                format!("Usage: {}in/{}out{}", input_tokens, output_tokens, cost),
            )
        }
        EventKind::Paused => ("||", Color::Yellow, "Paused".to_string()),
        EventKind::Resumed => ("|>", Color::Green, "Resumed".to_string()),
        EventKind::Inject { content } => {
            let truncated = if content.len() > 40 {
                format!("{}...", &content[..37])
            } else {
                content.clone()
            };
            ("^", Color::Cyan, format!("Inject: {}", truncated))
        }
        EventKind::Redirect { from, to } => {
            ("~", Color::Magenta, format!("Redirect: {} -> {}", from, to))
        }
        EventKind::FlowStart {
            flow_name,
            entry_node,
        } => (
            "[",
            Color::Blue,
            format!("Flow start: {} ({})", flow_name, entry_node),
        ),
        EventKind::FlowComplete { flow_name, .. } => {
            ("]", Color::Green, format!("Flow complete: {}", flow_name))
        }
        EventKind::Iteration {
            iteration,
            max_iterations,
        } => (
            ".",
            Color::DarkGray,
            format!("Iteration {}/{}", iteration + 1, max_iterations),
        ),
        EventKind::BashExec {
            command,
            exit_code,
            duration_ms,
            stdout,
            stderr,
        } => {
            let status = if *exit_code == 0 { "OK" } else { "FAIL" };
            let duration = duration_ms
                .map(|d| format!(" ({}ms)", d))
                .unwrap_or_default();
            let has_stderr = if !stderr.is_empty() { " +stderr" } else { "" };
            let out_len = stdout.len();
            let cmd_truncated = if command.len() > 30 {
                format!("{}...", &command[..27])
            } else {
                command.clone()
            };
            (
                "B",
                if *exit_code == 0 { Color::Green } else { Color::Red },
                format!(
                    "bash: {} [{}{}] {}b out{}",
                    cmd_truncated, status, duration, out_len, has_stderr
                ),
            )
        }
    }
}

/// Run the dashboard with default log directory
pub fn run_dashboard() -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("gsh")
        .join("logs");

    let mut dashboard = Dashboard::new(log_dir);
    dashboard.run()
}

/// Run the dashboard with a custom log directory
pub fn run_dashboard_with_dir(log_dir: PathBuf) -> Result<()> {
    let mut dashboard = Dashboard::new(log_dir);
    dashboard.run()
}
