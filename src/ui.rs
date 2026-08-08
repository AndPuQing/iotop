use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyEvent,
        KeyEventKind, MouseEvent,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState,
    },
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
    fn as_str(&self) -> &str {
        match self {
            SortColumn::Pid => "tid",
            SortColumn::Prio => "prio",
            SortColumn::User => "user",
            SortColumn::Read => "read",
            SortColumn::Write => "write",
            SortColumn::Swapin => "swapin",
            SortColumn::Io => "io",
            SortColumn::Command => "command",
        }
    }
}

/// Which columns are currently shown in the process table. Every column can be
/// toggled independently from the interactive column-selection mode, so users can
/// tailor the table to a narrow terminal or to the fields they care about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleColumns {
    pub tid: bool,
    pub prio: bool,
    pub user: bool,
    pub read: bool,
    pub write: bool,
    pub swapin: bool,
    pub io: bool,
    pub command: bool,
}

impl Default for VisibleColumns {
    fn default() -> Self {
        Self {
            tid: true,
            prio: true,
            user: true,
            read: true,
            write: true,
            swapin: true,
            io: true,
            command: true,
        }
    }
}

impl VisibleColumns {
    pub fn is_visible(&self, column: SortColumn) -> bool {
        match column {
            SortColumn::Pid => self.tid,
            SortColumn::Prio => self.prio,
            SortColumn::User => self.user,
            SortColumn::Read => self.read,
            SortColumn::Write => self.write,
            SortColumn::Swapin => self.swapin,
            SortColumn::Io => self.io,
            SortColumn::Command => self.command,
        }
    }

    pub fn set_visible(&mut self, column: SortColumn, visible: bool) {
        match column {
            SortColumn::Pid => self.tid = visible,
            SortColumn::Prio => self.prio = visible,
            SortColumn::User => self.user = visible,
            SortColumn::Read => self.read = visible,
            SortColumn::Write => self.write = visible,
            SortColumn::Swapin => self.swapin = visible,
            SortColumn::Io => self.io = visible,
            SortColumn::Command => self.command = visible,
        }
    }

    /// Number of columns currently both available and visible.
    pub fn count(&self, has_delay_acct: bool) -> usize {
        SortColumn::available_columns(has_delay_acct)
            .iter()
            .filter(|c| self.is_visible(**c))
            .count()
    }
}

impl SortColumn {
    /// Get all available columns based on whether delay accounting is available
    pub fn available_columns(has_delay_acct: bool) -> Vec<SortColumn> {
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

    pub fn cycle_forward(&self, has_delay_acct: bool) -> Self {
        let columns = Self::available_columns(has_delay_acct);
        let current_idx = columns.iter().position(|c| c == self);

        match current_idx {
            Some(idx) => {
                let next_idx = (idx + 1) % columns.len();
                columns[next_idx]
            }
            None => columns[0],
        }
    }

    pub fn cycle_backward(&self, has_delay_acct: bool) -> Self {
        let columns = Self::available_columns(has_delay_acct);
        let current_idx = columns.iter().position(|c| c == self);

        match current_idx {
            Some(idx) => {
                let prev_idx = if idx == 0 { columns.len() - 1 } else { idx - 1 };
                columns[prev_idx]
            }
            None => columns[0],
        }
    }
}

pub struct UIState {
    pub only_active: bool,
    pub accumulated: bool,
    pub sort_column: SortColumn,
    pub sort_reverse: bool,
    pub paused: bool,
    pub show_processes: bool,
    pub kilobytes: bool,
    pub column_mode: bool,
    pub column_cursor: SortColumn,
    pub visible_columns: VisibleColumns,
    pub scroll_offset: usize,
    pub selection_mode: bool,
    pub selected_row: Option<usize>,
    pub table_state: TableState,
}

impl Default for UIState {
    fn default() -> Self {
        Self {
            only_active: false,
            accumulated: false,
            sort_column: SortColumn::Pid,
            sort_reverse: true,
            paused: false,
            show_processes: false,
            kilobytes: false,
            column_mode: false,
            column_cursor: SortColumn::Pid,
            visible_columns: VisibleColumns::default(),
            scroll_offset: 0,
            selection_mode: false,
            selected_row: None,
            table_state: TableState::default(),
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
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            cursor::Hide,
            EnableMouseCapture
        )?;
        self.start();
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop()?;
        if crossterm::terminal::is_raw_mode_enabled()? {
            self.terminal.flush()?;
            execute!(
                io::stdout(),
                LeaveAlternateScreen,
                cursor::Show,
                DisableMouseCapture
            )?;
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
        state: &mut UIState,
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
    state: &mut UIState,
    has_delay_acct: bool,
) {
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header with time and I/O stats
            Constraint::Min(5),    // Process table
        ])
        .split(size);

    render_header(f, chunks[0], total_io, actual_io, duration, state.kilobytes);

    render_process_table(f, chunks[1], processes, duration, state, has_delay_acct);
}

fn render_header(
    f: &mut Frame,
    area: Rect,
    total_io: (u64, u64),
    actual_io: (u64, u64),
    duration: f64,
    kilobytes: bool,
) {
    let total_read_str = if kilobytes {
        format_bandwidth_kb(total_io.0, duration)
    } else {
        format_bandwidth(total_io.0, duration)
    };
    let total_write_str = if kilobytes {
        format_bandwidth_kb(total_io.1, duration)
    } else {
        format_bandwidth(total_io.1, duration)
    };
    let actual_read_str = if kilobytes {
        format_bandwidth_kb(actual_io.0, duration)
    } else {
        format_bandwidth(actual_io.0, duration)
    };
    let actual_write_str = if kilobytes {
        format_bandwidth_kb(actual_io.1, duration)
    } else {
        format_bandwidth(actual_io.1, duration)
    };

    let text = vec![
        Line::from(vec![
            Span::styled("Total DISK READ: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{:>12}", total_read_str),
                Style::default().fg(Color::White),
            ),
            Span::raw("  │  "),
            Span::styled("Total DISK WRITE: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{:>12}", total_write_str),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Actual DISK READ: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{:>11}", actual_read_str),
                Style::default().fg(Color::White),
            ),
            Span::raw("  │  "),
            Span::styled("Actual DISK WRITE: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{:>11}", actual_write_str),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    let block = Block::default()
        .title_top(
            Line::from(vec![
                Span::raw("┐"),
                Span::styled(
                    format!("{}", chrono::Local::now().format("%H:%M:%S")),
                    Style::default().fg(Color::White).bold(),
                ),
                Span::raw("┌"),
            ])
            .centered(),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Gray))
        .bg(Color::Black)
        .title(" iotop - I/O Monitor ");

    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

const COLOR_HIGHLIGHT: Color = Color::Rgb(100, 180, 255);
const COLOR_READ: Color = Color::Rgb(100, 180, 255); // Soft blue
const COLOR_WRITE: Color = Color::Rgb(255, 140, 140); // Soft red/pink
const COLOR_IO: Color = Color::Rgb(180, 140, 255); // Soft purple

fn column_header(column: SortColumn) -> (&'static str, Alignment) {
    match column {
        SortColumn::Pid => ("TID:", Alignment::Right),
        SortColumn::Prio => ("PRIO:", Alignment::Right),
        SortColumn::User => ("USER:", Alignment::Left),
        SortColumn::Read => ("DISK READ:", Alignment::Right),
        SortColumn::Write => ("DISK WRITE:", Alignment::Right),
        SortColumn::Swapin => ("SWAPIN:", Alignment::Right),
        SortColumn::Io => ("IO:", Alignment::Right),
        SortColumn::Command => ("COMMAND:", Alignment::Left),
    }
}

fn column_width(column: SortColumn) -> Constraint {
    match column {
        SortColumn::Pid => Constraint::Length(8),
        SortColumn::Prio => Constraint::Length(7),
        SortColumn::User => Constraint::Length(9),
        SortColumn::Read | SortColumn::Write => Constraint::Length(14),
        SortColumn::Swapin => Constraint::Length(9),
        SortColumn::Io => Constraint::Length(5),
        SortColumn::Command => Constraint::Min(20),
    }
}

/// Read bandwidth/size string for a process, honoring the KB-unit toggle.
fn read_str(process: &ProcessInfo, state: &UIState, duration: f64) -> String {
    let stats = if state.accumulated {
        &process.stats_accum
    } else {
        &process.stats_delta
    };
    if state.kilobytes {
        if state.accumulated {
            format_size_kb(stats.read_bytes)
        } else {
            format_bandwidth_kb(stats.read_bytes, duration)
        }
    } else if state.accumulated {
        human_size(stats.read_bytes as i64)
    } else {
        format_bandwidth(stats.read_bytes, duration)
    }
}

/// Write bandwidth/size string for a process, honoring the KB-unit toggle.
fn write_str(process: &ProcessInfo, state: &UIState, duration: f64) -> String {
    let stats = if state.accumulated {
        &process.stats_accum
    } else {
        &process.stats_delta
    };
    let write_bytes = stats
        .write_bytes
        .saturating_sub(stats.cancelled_write_bytes);
    if state.kilobytes {
        if state.accumulated {
            format_size_kb(write_bytes)
        } else {
            format_bandwidth_kb(write_bytes, duration)
        }
    } else if state.accumulated {
        human_size(write_bytes as i64)
    } else {
        format_bandwidth(write_bytes, duration)
    }
}

/// Build the data cell for a process for a single column.
fn process_cell<'a>(
    process: &'a ProcessInfo,
    column: SortColumn,
    state: &UIState,
    duration: f64,
) -> Cell<'a> {
    let stats = if state.accumulated {
        &process.stats_accum
    } else {
        &process.stats_delta
    };
    match column {
        SortColumn::Pid => {
            Cell::from(Text::from(process.tid.to_string()).alignment(Alignment::Right))
        }
        SortColumn::Prio => {
            Cell::from(Text::from(process.get_prio().to_string()).alignment(Alignment::Right))
        }
        SortColumn::User => Cell::from(Text::from(process.get_user()).alignment(Alignment::Left)),
        SortColumn::Read => {
            Cell::from(Text::from(read_str(process, state, duration)).alignment(Alignment::Right))
                .style(Style::default().fg(COLOR_READ))
        }
        SortColumn::Write => {
            Cell::from(Text::from(write_str(process, state, duration)).alignment(Alignment::Right))
                .style(Style::default().fg(COLOR_WRITE))
        }
        SortColumn::Swapin => Cell::from(
            Text::from(format_delay_percent(stats.swapin_delay_total, duration))
                .alignment(Alignment::Right),
        ),
        SortColumn::Io => Cell::from(
            Text::from(format_delay_percent(stats.blkio_delay_total, duration))
                .alignment(Alignment::Right),
        )
        .style(Style::default().fg(COLOR_IO)),
        SortColumn::Command => {
            Cell::from(Text::from(process.get_cmdline()).alignment(Alignment::Left))
        }
    }
}

/// Build a header cell for a column, highlighting it when it is the column
/// currently selected in column-selection mode.
fn header_cell(column: SortColumn, state: &UIState) -> Cell<'static> {
    let (text, align) = column_header(column);
    let cell = Cell::from(Text::from(text).alignment(align));
    if state.column_mode && state.column_cursor == column {
        cell.style(
            Style::default()
                .fg(Color::Black)
                .bg(COLOR_HIGHLIGHT)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        cell
    }
}

fn create_toggle_title(hotkey: char, label: &'static str, is_active: bool) -> Line<'static> {
    let base_style = Style::default().fg(COLOR_HIGHLIGHT);
    let active_style = base_style.bold();

    Line::from(vec![
        Span::raw("┐"),
        if is_active {
            Span::styled(hotkey.to_string(), active_style)
        } else {
            Span::styled(hotkey.to_string(), base_style)
        },
        if is_active {
            Span::raw(label).bold()
        } else {
            Span::raw(label)
        },
        Span::raw("┌"),
    ])
    .left_aligned()
}

/// Title bar shown in column-selection mode: lists every column with its current
/// visibility and marks the column under the cursor. Hidden columns are shown
/// dimmed with `~` markers; the cursor column gets a highlighted background.
fn column_mode_title(state: &UIState, has_delay_acct: bool) -> Line<'static> {
    let mut spans = vec![
        Span::raw("┐ "),
        Span::styled("cols", Style::default().bold()),
    ];
    for column in SortColumn::available_columns(has_delay_acct) {
        let name = column.as_str();
        let visible = state.visible_columns.is_visible(column);
        let is_cursor = state.column_cursor == column;

        let text = if visible {
            format!(" {} ", name)
        } else {
            format!("~{}~ ", name)
        };
        let span = if is_cursor {
            Span::styled(
                text,
                Style::default().fg(Color::Black).bg(COLOR_HIGHLIGHT).bold(),
            )
        } else if visible {
            Span::styled(text, Style::default().fg(Color::Gray))
        } else {
            Span::styled(text, Style::default().fg(Color::DarkGray))
        };
        spans.push(span);
    }
    spans.push(Span::styled(
        "  ←/→ move · space toggle · esc/c done ┌",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    Line::from(spans).left_aligned()
}

fn render_process_table(
    f: &mut Frame,
    area: Rect,
    processes: &[&ProcessInfo],
    duration: f64,
    state: &mut UIState,
    has_delay_acct: bool,
) {
    let header_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    // Columns to render, filtered by user visibility preferences.
    let columns: Vec<SortColumn> = SortColumn::available_columns(has_delay_acct)
        .into_iter()
        .filter(|c| state.visible_columns.is_visible(*c))
        .collect();

    let header_cells: Vec<Cell> = columns.iter().map(|c| header_cell(*c, state)).collect();
    let header = Row::new(header_cells).style(header_style).height(1);

    let available_height = area.height.saturating_sub(3) as usize;
    let total_processes = processes.len();

    if total_processes > 0 {
        let max_scroll = total_processes.saturating_sub(available_height);
        state.scroll_offset = state.scroll_offset.min(max_scroll);
    } else {
        state.scroll_offset = 0;
    }

    let end = (state.scroll_offset + available_height).min(total_processes);
    let visible_processes = &processes[state.scroll_offset..end];

    const COLOR_ACTIVE: Color = Color::White;
    const COLOR_INACTIVE: Color = Color::Gray;

    let rows = visible_processes.iter().map(|process| {
        let row_style = if process.did_some_io(state.accumulated) {
            Style::default().fg(COLOR_ACTIVE)
        } else {
            Style::default().fg(COLOR_INACTIVE)
        };

        let cells: Vec<Cell> = columns
            .iter()
            .map(|c| process_cell(process, *c, state, duration))
            .collect();

        Row::new(cells).style(row_style)
    });

    let widths: Vec<Constraint> = columns.iter().map(|c| column_width(*c)).collect();

    let sort_row = state.sort_column.as_str();

    let scroll_indicator = if total_processes > available_height {
        let start_row = state.scroll_offset + 1;
        let end_row = end.min(total_processes);
        let percentage = if total_processes > 0 {
            ((state.scroll_offset as f64 / total_processes as f64) * 100.0) as usize
        } else {
            0
        };
        format!(
            "{}-{}/{} ({}%)",
            start_row, end_row, total_processes, percentage
        )
    } else {
        String::new()
    };

    let mut block = Block::default()
        .title_top(create_toggle_title('a', "ccumulated", state.accumulated))
        .title_top(create_toggle_title('o', "nly-active", state.only_active))
        .title_top(create_toggle_title('p', "rocesses", state.show_processes))
        .title_top(create_toggle_title('r', "everse", !state.sort_reverse))
        .title_top(create_toggle_title('k', "ilobytes", state.kilobytes))
        .title_top(
            Line::from(vec![
                Span::raw("┐"),
                Span::styled("← ", Style::default().fg(COLOR_HIGHLIGHT).bold()),
                Span::raw(sort_row).bold(),
                Span::styled(" →", Style::default().fg(COLOR_HIGHLIGHT).bold()),
                Span::raw("┌"),
            ])
            .left_aligned(),
        )
        .bg(Color::Black)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Gray));

    if state.column_mode {
        block = block.title_top(column_mode_title(state, has_delay_acct));
    }

    if !scroll_indicator.is_empty() {
        block = block.title_top(
            Line::from(vec![
                Span::raw("┐"),
                Span::styled(
                    scroll_indicator,
                    Style::default().fg(COLOR_HIGHLIGHT).bold(),
                ),
                Span::raw("┌"),
            ])
            .right_aligned(),
        );
    }

    let table = Table::default()
        .rows(rows)
        .header(header)
        .widths(widths)
        .block(block)
        .row_highlight_style(Style::default().bg(Color::Rgb(60, 60, 60))); // Dark gray background for selection

    // Update table_state to reflect current selection
    if state.selection_mode {
        if let Some(selected) = state.selected_row {
            // selected_row is the absolute index, but TableState needs the visible index
            let visible_idx = selected.saturating_sub(state.scroll_offset);
            state.table_state.select(Some(visible_idx));
        }
    } else {
        state.table_state.select(None);
    }

    f.render_stateful_widget(table, area, &mut state.table_state);

    if total_processes > available_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some(" "))
            .thumb_symbol("█")
            .style(Style::default().fg(COLOR_HIGHLIGHT));

        let mut scrollbar_state = ScrollbarState::new(total_processes / available_height)
            .position(state.scroll_offset / available_height)
            .viewport_content_length(1);

        f.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

pub fn format_bandwidth(bytes: u64, duration: f64) -> String {
    if duration <= 0.0 {
        return "0 B/s".to_string();
    }
    let bytes_per_sec = bytes as f64 / duration;
    human_size(bytes_per_sec as i64) + "/s"
}

pub fn format_bandwidth_kb(bytes: u64, duration: f64) -> String {
    if duration <= 0.0 {
        return "0.00 K/s".to_string();
    }
    let kb_per_sec = (bytes as f64 / duration) / 1024.0;
    format!("{:.2} K/s", kb_per_sec)
}

pub fn format_size_kb(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    format!("{:.2} K", kb)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.00 K");
        assert_eq!(human_size(1536), "1.50 K");
        assert_eq!(human_size(1024 * 1024), "1.00 M");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.00 G");
        assert_eq!(human_size(15 * 1024), "15.0 K");
    }

    #[test]
    fn test_format_bandwidth() {
        assert_eq!(format_bandwidth(0, 1.0), "0 B/s");
        assert_eq!(format_bandwidth(1024, 1.0), "1.00 K/s");
        assert_eq!(format_bandwidth(1024 * 1024, 1.0), "1.00 M/s");
        assert_eq!(format_bandwidth(2048, 2.0), "1.00 K/s");
        assert_eq!(format_bandwidth(1024, 0.0), "0 B/s");
    }

    #[test]
    fn test_format_bandwidth_kb() {
        assert_eq!(format_bandwidth_kb(0, 1.0), "0.00 K/s");
        assert_eq!(format_bandwidth_kb(1024, 1.0), "1.00 K/s");
        assert_eq!(format_bandwidth_kb(2048, 2.0), "1.00 K/s");
    }

    #[test]
    fn test_format_size_kb() {
        assert_eq!(format_size_kb(0), "0.00 K");
        assert_eq!(format_size_kb(1024), "1.00 K");
        assert_eq!(format_size_kb(2048), "2.00 K");
    }

    #[test]
    fn test_format_delay_percent() {
        assert_eq!(format_delay_percent(0, 1.0), "0.00 %");
        assert_eq!(format_delay_percent(1_000_000_000, 1.0), "100.00 %");
        assert_eq!(format_delay_percent(500_000_000, 1.0), "50.00 %");
        assert_eq!(format_delay_percent(100_000_000, 2.0), "5.00 %");
        assert_eq!(format_delay_percent(100, 0.0), "0.00 %");
    }

    #[test]
    fn test_sort_column_cycle() {
        let col = SortColumn::Pid;
        assert_eq!(col.cycle_forward(true), SortColumn::Prio);
        assert_eq!(col.cycle_backward(true), SortColumn::Command);

        let col = SortColumn::Command;
        assert_eq!(col.cycle_forward(true), SortColumn::Pid);
        assert_eq!(col.cycle_backward(true), SortColumn::Io);

        let col = SortColumn::Read;
        let next = col.cycle_forward(false);
        assert_eq!(next, SortColumn::Write);
    }

    #[test]
    fn test_ui_state_default() {
        let state = UIState::default();
        assert!(!state.only_active);
        assert!(!state.accumulated);
        assert_eq!(state.sort_column, SortColumn::Pid);
        assert!(state.sort_reverse);
        assert!(!state.paused);
        assert!(!state.show_processes);
        assert!(!state.kilobytes);
        assert!(!state.column_mode);
        assert_eq!(state.column_cursor, SortColumn::Pid);
        assert_eq!(state.visible_columns.count(true), 8);
        assert_eq!(state.visible_columns.count(false), 6);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_visible_columns_toggle() {
        let mut vis = VisibleColumns::default();
        assert!(vis.is_visible(SortColumn::Read));
        vis.set_visible(SortColumn::Read, false);
        assert!(!vis.is_visible(SortColumn::Read));
        assert_eq!(vis.count(true), 7);
        vis.set_visible(SortColumn::Read, true);
        assert_eq!(vis.count(true), 8);
    }
}
