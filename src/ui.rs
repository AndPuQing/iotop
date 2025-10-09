use anyhow::Result;
use crossterm::{
    cursor,
    event::{Event as CrosstermEvent, EventStream, KeyEvent, KeyEventKind, MouseEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::ops::{Deref, DerefMut};
use std::time::Duration;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::process::ProcessInfo;

#[derive(Debug, Clone)]
pub enum Event {
    Init,
    #[allow(dead_code)]
    Quit,
    Error,
    Tick,
    Render,
    Key(KeyEvent),
    #[allow(dead_code)]
    Mouse(MouseEvent),
    #[allow(dead_code)]
    Resize(u16, u16),
    DataUpdate(crate::process::ProcessSnapshot),
}

pub struct Tui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    pub task: JoinHandle<()>,
    pub cancellation_token: CancellationToken,
    pub event_rx: UnboundedReceiver<Event>,
    pub event_tx: UnboundedSender<Event>,
    pub frame_rate: f64,
    pub tick_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortColumn {
    Pid,
    Prio,
    User,
    Read,
    Write,
    Swapin,
    Io,
    Command,
}

impl SortColumn {
    /// Get all available columns based on whether delay accounting is available
    fn available_columns(has_delay_acct: bool) -> Vec<SortColumn> {
        if has_delay_acct {
            vec![
                SortColumn::Pid,
                SortColumn::Prio,
                SortColumn::User,
                SortColumn::Read,
                SortColumn::Write,
                SortColumn::Swapin,
                SortColumn::Io,
                SortColumn::Command,
            ]
        } else {
            vec![
                SortColumn::Pid,
                SortColumn::Prio,
                SortColumn::User,
                SortColumn::Read,
                SortColumn::Write,
                SortColumn::Command,
            ]
        }
    }

    /// Cycle to the next column (right arrow)
    pub fn cycle_forward(&self, has_delay_acct: bool) -> Self {
        let columns = Self::available_columns(has_delay_acct);
        let current_idx = columns.iter().position(|c| c == self);

        match current_idx {
            Some(idx) => {
                let next_idx = (idx + 1) % columns.len();
                columns[next_idx]
            }
            None => {
                // Current column not in available list, return first available
                columns[0]
            }
        }
    }

    /// Cycle to the previous column (left arrow)
    pub fn cycle_backward(&self, has_delay_acct: bool) -> Self {
        let columns = Self::available_columns(has_delay_acct);
        let current_idx = columns.iter().position(|c| c == self);

        match current_idx {
            Some(idx) => {
                let prev_idx = if idx == 0 { columns.len() - 1 } else { idx - 1 };
                columns[prev_idx]
            }
            None => {
                // Current column not in available list, return first available
                columns[0]
            }
        }
    }
}

pub struct UIState {
    pub only_active: bool,
    pub accumulated: bool,
    pub sort_column: SortColumn,
    pub sort_reverse: bool,
    pub paused: bool,
}

impl Default for UIState {
    fn default() -> Self {
        Self {
            only_active: false,
            accumulated: false,
            sort_column: SortColumn::Io,
            sort_reverse: true,
            paused: false,
        }
    }
}

impl Tui {
    pub fn new() -> Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            task: tokio::spawn(async {}),
            cancellation_token: CancellationToken::new(),
            event_rx,
            event_tx,
            frame_rate: 60.0,
            tick_rate: 1.0, // 1 Hz for iotop data updates
        })
    }

    pub fn start(&mut self) {
        self.cancel(); // Cancel any existing task
        self.cancellation_token = CancellationToken::new();
        let event_loop = Self::event_loop(
            self.event_tx.clone(),
            self.cancellation_token.clone(),
            self.tick_rate,
            self.frame_rate,
        );
        self.task = tokio::spawn(async {
            event_loop.await;
        });
    }

    async fn event_loop(
        event_tx: UnboundedSender<Event>,
        cancellation_token: CancellationToken,
        tick_rate: f64,
        frame_rate: f64,
    ) {
        let mut event_stream = EventStream::new();
        let mut tick_interval = interval(Duration::from_secs_f64(1.0 / tick_rate));
        let mut render_interval = interval(Duration::from_secs_f64(1.0 / frame_rate));

        // Send init event
        let _ = event_tx.send(Event::Init);

        loop {
            let event = tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                }
                _ = tick_interval.tick() => Event::Tick,
                _ = render_interval.tick() => Event::Render,
                crossterm_event = event_stream.next().fuse() => match crossterm_event {
                    Some(Ok(event)) => match event {
                        CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => Event::Key(key),
                        CrosstermEvent::Mouse(mouse) => Event::Mouse(mouse),
                        CrosstermEvent::Resize(x, y) => Event::Resize(x, y),
                        _ => continue, // ignore other events
                    }
                    Some(Err(_)) => Event::Error,
                    None => break, // the event stream has stopped
                },
            };
            if event_tx.send(event).is_err() {
                // the receiver has been dropped
                break;
            }
        }
        cancellation_token.cancel();
    }

    pub fn stop(&self) -> Result<()> {
        self.cancel();
        let mut counter = 0;
        while !self.task.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
            counter += 1;
            if counter > 50 {
                self.task.abort();
            }
            if counter > 100 {
                break;
            }
        }
        Ok(())
    }

    pub fn enter(&mut self) -> Result<()> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        self.start();
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop()?;
        if crossterm::terminal::is_raw_mode_enabled()? {
            self.terminal.flush()?;
            execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
            disable_raw_mode()?;
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }

    pub fn draw(
        &mut self,
        processes: &[&ProcessInfo],
        total_io: (u64, u64),
        actual_io: (u64, u64),
        duration: f64,
        state: &UIState,
        has_delay_acct: bool,
    ) -> Result<()> {
        self.terminal.draw(|f| {
            render_ui(
                f,
                processes,
                total_io,
                actual_io,
                duration,
                state,
                has_delay_acct,
            );
        })?;
        Ok(())
    }
}

impl Deref for Tui {
    type Target = Terminal<CrosstermBackend<Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for Tui {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

fn render_ui(
    f: &mut Frame,
    processes: &[&ProcessInfo],
    total_io: (u64, u64),
    actual_io: (u64, u64),
    duration: f64,
    state: &UIState,
    has_delay_acct: bool,
) {
    let size = f.area();

    // Create main layout: header + content + footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header with I/O stats
            Constraint::Min(5),    // Process table
            Constraint::Length(3), // Footer with help
        ])
        .split(size);

    // Render header
    render_header(f, chunks[0], total_io, actual_io, duration);

    // Render process table
    render_process_table(f, chunks[1], processes, duration, state, has_delay_acct);

    // Render footer
    render_footer(f, chunks[2], state);
}

fn render_header(
    f: &mut Frame,
    area: Rect,
    total_io: (u64, u64),
    actual_io: (u64, u64),
    duration: f64,
) {
    let total_read_str = format_bandwidth(total_io.0, duration);
    let total_write_str = format_bandwidth(total_io.1, duration);
    let actual_read_str = format_bandwidth(actual_io.0, duration);
    let actual_write_str = format_bandwidth(actual_io.1, duration);

    let text = vec![
        Line::from(vec![
            Span::styled(
                "Total DISK READ: ",
                Style::default().fg(Color::Rgb(180, 180, 180)),
            ),
            Span::styled(
                format!("{:>12}", total_read_str),
                Style::default().fg(Color::Rgb(100, 180, 255)), // Soft blue
            ),
            Span::raw("  │  "),
            Span::styled(
                "Total DISK WRITE: ",
                Style::default().fg(Color::Rgb(180, 180, 180)),
            ),
            Span::styled(
                format!("{:>12}", total_write_str),
                Style::default().fg(Color::Rgb(255, 140, 140)), // Soft red/pink
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Actual DISK READ: ",
                Style::default().fg(Color::Rgb(140, 140, 140)),
            ),
            Span::styled(
                format!("{:>11}", actual_read_str),
                Style::default().fg(Color::Rgb(100, 180, 255)), // Soft blue
            ),
            Span::raw("  │  "),
            Span::styled(
                "Actual DISK WRITE: ",
                Style::default().fg(Color::Rgb(140, 140, 140)),
            ),
            Span::styled(
                format!("{:>11}", actual_write_str),
                Style::default().fg(Color::Rgb(255, 140, 140)), // Soft red/pink
            ),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(100, 100, 100))) // Gray borders
        .title(" iotop - I/O Monitor ");

    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

fn render_process_table(
    f: &mut Frame,
    area: Rect,
    processes: &[&ProcessInfo],
    duration: f64,
    state: &UIState,
    has_delay_acct: bool,
) {
    let header_style = Style::default()
        .fg(Color::Rgb(200, 200, 200)) // Light gray
        .add_modifier(Modifier::BOLD);

    let sort_indicator = if state.sort_reverse { "▼" } else { "▲" };

    let header_cells = if has_delay_acct {
        vec![
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Pid {
                    format!("TID {}", sort_indicator)
                } else {
                    "TID".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Prio {
                    format!("PRIO {}", sort_indicator)
                } else {
                    "PRIO".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::User {
                    format!("USER {}", sort_indicator)
                } else {
                    "USER".to_string()
                })
                .alignment(Alignment::Left),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Read {
                    format!("DISK READ {}", sort_indicator)
                } else {
                    "DISK READ".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Write {
                    format!("DISK WRITE {}", sort_indicator)
                } else {
                    "DISK WRITE".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Swapin {
                    format!("SWAPIN {}", sort_indicator)
                } else {
                    "SWAPIN".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Io {
                    format!("IO {}", sort_indicator)
                } else {
                    "IO".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Command {
                    format!("COMMAND {}", sort_indicator)
                } else {
                    "COMMAND".to_string()
                })
                .alignment(Alignment::Left),
            ),
        ]
    } else {
        vec![
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Pid {
                    format!("TID {}", sort_indicator)
                } else {
                    "TID".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Prio {
                    format!("PRIO {}", sort_indicator)
                } else {
                    "PRIO".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::User {
                    format!("USER {}", sort_indicator)
                } else {
                    "USER".to_string()
                })
                .alignment(Alignment::Left),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Read {
                    format!("DISK READ {}", sort_indicator)
                } else {
                    "DISK READ".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Write {
                    format!("DISK WRITE {}", sort_indicator)
                } else {
                    "DISK WRITE".to_string()
                })
                .alignment(Alignment::Right),
            ),
            Cell::from(
                Text::from(if state.sort_column == SortColumn::Command {
                    format!("COMMAND {}", sort_indicator)
                } else {
                    "COMMAND".to_string()
                })
                .alignment(Alignment::Left),
            ),
        ]
    };

    let header = Row::new(header_cells).style(header_style).height(1);

    let rows = processes.iter().map(|process| {
        let stats = if state.accumulated {
            &process.stats_accum
        } else {
            &process.stats_delta
        };

        let read_str = if state.accumulated {
            human_size(stats.read_bytes as i64)
        } else {
            format_bandwidth(stats.read_bytes, duration)
        };

        let write_bytes = stats
            .write_bytes
            .saturating_sub(stats.cancelled_write_bytes);
        let write_str = if state.accumulated {
            human_size(write_bytes as i64)
        } else {
            format_bandwidth(write_bytes, duration)
        };

        let row_style = if process.did_some_io(state.accumulated) {
            // Active processes - white/light gray
            Style::default().fg(Color::Rgb(220, 220, 220))
        } else {
            // Inactive processes - darker gray
            Style::default().fg(Color::Rgb(100, 100, 100))
        };

        if has_delay_acct {
            let io_delay = format_delay_percent(stats.blkio_delay_total, duration);
            let swapin_delay = format_delay_percent(stats.swapin_delay_total, duration);

            Row::new(vec![
                Cell::from(Text::from(process.tid.to_string()).alignment(Alignment::Right)),
                Cell::from(Text::from(process.get_prio().to_string()).alignment(Alignment::Right)),
                Cell::from(Text::from(process.get_user()).alignment(Alignment::Left)),
                Cell::from(Text::from(read_str).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Rgb(100, 180, 255))), // Soft blue
                Cell::from(Text::from(write_str).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Rgb(255, 140, 140))), // Soft red/pink
                Cell::from(Text::from(swapin_delay).alignment(Alignment::Right)),
                Cell::from(Text::from(io_delay).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Rgb(180, 140, 255))), // Soft purple
                Cell::from(Text::from(process.get_cmdline()).alignment(Alignment::Left)),
            ])
            .style(row_style)
        } else {
            Row::new(vec![
                Cell::from(Text::from(process.tid.to_string()).alignment(Alignment::Right)),
                Cell::from(Text::from(process.get_prio().to_string()).alignment(Alignment::Right)),
                Cell::from(Text::from(process.get_user()).alignment(Alignment::Left)),
                Cell::from(Text::from(read_str).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Rgb(100, 180, 255))), // Soft blue
                Cell::from(Text::from(write_str).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Rgb(255, 140, 140))), // Soft red/pink
                Cell::from(Text::from(process.get_cmdline()).alignment(Alignment::Left)),
            ])
            .style(row_style)
        }
    });

    let widths = if has_delay_acct {
        vec![
            Constraint::Length(8),  // TID
            Constraint::Length(7),  // PRIO (needs space for "PRIO ▼")
            Constraint::Length(9),  // USER
            Constraint::Length(14), // DISK READ (needs space for "DISK READ ▼")
            Constraint::Length(14), // DISK WRITE (needs space for "DISK WRITE ▼")
            Constraint::Length(9),  // SWAPIN (needs space for "SWAPIN ▼")
            Constraint::Length(5),  // IO (needs space for "IO ▼")
            Constraint::Min(20),    // COMMAND
        ]
    } else {
        vec![
            Constraint::Length(8),  // TID
            Constraint::Length(7),  // PRIO (needs space for "PRIO ▼")
            Constraint::Length(9),  // USER
            Constraint::Length(14), // DISK READ (needs space for "DISK READ ▼")
            Constraint::Length(14), // DISK WRITE (needs space for "DISK WRITE ▼")
            Constraint::Min(20),    // COMMAND
        ]
    };

    let table = Table::default()
        .rows(rows)
        .header(header)
        .widths(widths)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(100, 100, 100))), // Gray borders
        );

    f.render_widget(table, area);
}

fn render_footer(f: &mut Frame, area: Rect, state: &UIState) {
    let help_items = vec![
        Span::styled("q", Style::default().fg(Color::Rgb(100, 180, 255)).bold()), // Soft blue
        Span::raw(":quit  "),
        Span::styled("o", Style::default().fg(Color::Rgb(100, 180, 255)).bold()),
        Span::raw(":only-active  "),
        Span::styled("a", Style::default().fg(Color::Rgb(100, 180, 255)).bold()),
        Span::raw(":accumulated  "),
        Span::styled("←→", Style::default().fg(Color::Rgb(100, 180, 255)).bold()),
        Span::raw(":sort  "),
        Span::styled("↑↓", Style::default().fg(Color::Rgb(100, 180, 255)).bold()),
        Span::raw(":reverse  "),
        Span::styled(
            "space",
            Style::default().fg(Color::Rgb(100, 180, 255)).bold(),
        ),
        Span::raw(":pause  "),
    ];

    let status_items = vec![
        if state.only_active {
            Span::styled("[ACTIVE]", Style::default().fg(Color::Rgb(100, 180, 255)))
        // Soft blue
        } else {
            Span::styled("[ALL]", Style::default().fg(Color::Rgb(120, 120, 120)))
            // Medium gray
        },
        Span::raw(" "),
        if state.accumulated {
            Span::styled("[ACCUM]", Style::default().fg(Color::Rgb(180, 140, 255)))
        // Soft purple
        } else {
            Span::styled("[RATE]", Style::default().fg(Color::Rgb(120, 120, 120)))
            // Medium gray
        },
        Span::raw(" "),
        if state.paused {
            Span::styled("[PAUSED]", Style::default().fg(Color::Rgb(255, 140, 140)))
        // Soft red
        } else {
            Span::styled("[LIVE]", Style::default().fg(Color::Rgb(100, 180, 255)))
            // Soft blue
        },
    ];

    let text = vec![Line::from(help_items), Line::from(status_items)];

    let paragraph = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(100, 100, 100))) // Gray borders
            .title(" Controls "),
    );

    f.render_widget(paragraph, area);
}

pub fn format_bandwidth(bytes: u64, duration: f64) -> String {
    if duration <= 0.0 {
        return "0 B/s".to_string();
    }
    let bytes_per_sec = bytes as f64 / duration;
    human_size(bytes_per_sec as i64) + "/s"
}

pub fn human_size(bytes: i64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{:.0} {}", size, UNITS[unit_idx])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

pub fn format_delay_percent(delay_ns: u64, duration: f64) -> String {
    if duration <= 0.0 {
        return "0.00 %".to_string();
    }
    let percent = (delay_ns as f64 / (duration * 1_000_000_000.0)) * 100.0;
    format!("{:.2} %", percent)
}
