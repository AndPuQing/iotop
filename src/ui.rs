use crate::process::ProcessInfo;
use crate::taskstats::TaskStats;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Write};
use std::time::Duration;

const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P", "E"];

pub fn human_size(size: i64) -> String {
    let (sign, size) = if size < 0 {
        ("-", -size as f64)
    } else {
        ("", size as f64)
    };

    if size == 0.0 {
        return "0.00 B".to_string();
    }

    let expo = ((size / 2.0).log2() / 10.0) as usize;
    let expo = expo.min(UNITS.len() - 1);

    format!(
        "{}{:.2} {}",
        sign,
        size / (1u64 << (10 * expo)) as f64,
        UNITS[expo]
    )
}

pub fn format_bandwidth(bytes: u64, duration: f64) -> String {
    if duration > 0.0 {
        format!("{}/s", human_size((bytes as f64 / duration) as i64))
    } else {
        "0.00 B/s".to_string()
    }
}

pub fn format_delay_percent(delay_ns: u64, duration: f64) -> String {
    let percent = if duration > 0.0 {
        (delay_ns as f64 / (duration * 10_000_000.0)).min(99.99)
    } else {
        0.0
    };
    format!("{:.2} %", percent)
}

#[derive(Debug, Clone, Copy)]
pub enum SortColumn {
    Pid,
    User,
    Read,
    Write,
    Swapin,
    Io,
    Command,
}

impl SortColumn {
    pub fn next(&self) -> Self {
        match self {
            Self::Pid => Self::User,
            Self::User => Self::Read,
            Self::Read => Self::Write,
            Self::Write => Self::Swapin,
            Self::Swapin => Self::Io,
            Self::Io => Self::Command,
            Self::Command => Self::Command,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Pid => Self::Pid,
            Self::User => Self::Pid,
            Self::Read => Self::User,
            Self::Write => Self::Read,
            Self::Swapin => Self::Write,
            Self::Io => Self::Swapin,
            Self::Command => Self::Io,
        }
    }
}

pub struct UI {
    width: u16,
    height: u16,
    sort_column: SortColumn,
    sort_reverse: bool,
    show_only_active: bool,
    accumulated: bool,
}

impl UI {
    pub fn new() -> io::Result<Self> {
        let (width, height) = terminal::size()?;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        terminal::enable_raw_mode()?;

        Ok(Self {
            width,
            height,
            sort_column: SortColumn::Io,
            sort_reverse: true,
            show_only_active: false,
            accumulated: false,
        })
    }

    pub fn cleanup(&self) -> io::Result<()> {
        terminal::disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, Show)?;
        Ok(())
    }

    pub fn handle_input(&mut self) -> io::Result<bool> {
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        self.sort_reverse = !self.sort_reverse
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => self.accumulated = !self.accumulated,
                    KeyCode::Char('o') | KeyCode::Char('O') => {
                        self.show_only_active = !self.show_only_active
                    }
                    KeyCode::Left => self.sort_column = self.sort_column.prev(),
                    KeyCode::Right => self.sort_column = self.sort_column.next(),
                    _ => {}
                }
            }
        }
        Ok(false)
    }

    pub fn render(
        &mut self,
        processes: &mut Vec<&ProcessInfo>,
        total: (u64, u64),
        actual: (u64, u64),
        duration: f64,
    ) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        self.width = width;
        self.height = height;

        let mut stdout = io::stdout();
        execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;

        // Summary lines
        writeln!(
            stdout,
            "Total DISK READ :   {:>14} | Total DISK WRITE :   {:>14}",
            format_bandwidth(total.0, duration),
            format_bandwidth(total.1, duration)
        )?;
        writeln!(
            stdout,
            "Actual DISK READ:   {:>14} | Actual DISK WRITE:   {:>14}",
            format_bandwidth(actual.0, duration),
            format_bandwidth(actual.1, duration)
        )?;

        // Title line
        execute!(stdout, MoveTo(0, 3))?;
        let has_delay = TaskStats::has_delay_acct();
        if has_delay {
            write!(
                stdout,
                "{:>7}  {:>4}  {:<8}     {:>10}  {:>11}  {:>6}      {:>2}    COMMAND",
                "TID", "PRIO", "USER", "DISK READ", "DISK WRITE", "SWAPIN", "IO"
            )?;
        } else {
            write!(
                stdout,
                "{:>7}  {:>4}  {:<8}     {:>10}  {:>11} {} COMMAND",
                "TID", "PRIO", "USER", "DISK READ", "DISK WRITE", "?unavailable?"
            )?;
        }

        // Filter and sort processes
        if self.show_only_active {
            processes.retain(|p| p.did_some_io(self.accumulated));
        }

        let sort_col = self.sort_column;
        let accumulated = self.accumulated;
        let sort_reverse = self.sort_reverse;
        processes.sort_by(|a, b| {
            let stats_a = if accumulated {
                &a.stats_accum
            } else {
                &a.stats_delta
            };
            let stats_b = if accumulated {
                &b.stats_accum
            } else {
                &b.stats_delta
            };

            let cmp = match sort_col {
                SortColumn::Pid => a.tid.cmp(&b.tid),
                SortColumn::User => a.get_user().cmp(&b.get_user()),
                SortColumn::Read => stats_a.read_bytes.cmp(&stats_b.read_bytes),
                SortColumn::Write => {
                    let write_a = stats_a
                        .write_bytes
                        .saturating_sub(stats_a.cancelled_write_bytes);
                    let write_b = stats_b
                        .write_bytes
                        .saturating_sub(stats_b.cancelled_write_bytes);
                    write_a.cmp(&write_b)
                }
                SortColumn::Swapin => stats_a.swapin_delay_total.cmp(&stats_b.swapin_delay_total),
                SortColumn::Io => stats_a.blkio_delay_total.cmp(&stats_b.blkio_delay_total),
                SortColumn::Command => a.get_cmdline().cmp(&b.get_cmdline()),
            };

            let primary = if sort_reverse { cmp.reverse() } else { cmp };

            // Group by parent PID (TGID), then sort by TID within each group
            primary
                .then_with(|| a.pid.cmp(&b.pid))
                .then_with(|| a.tid.cmp(&b.tid))
        });

        // Display processes
        let max_lines = (height as usize).saturating_sub(5);
        for (i, process) in processes.iter().take(max_lines).enumerate() {
            execute!(stdout, MoveTo(0, 4 + i as u16))?;

            let stats = if self.accumulated {
                &process.stats_accum
            } else {
                &process.stats_delta
            };

            let read_str = if accumulated {
                human_size(stats.read_bytes as i64)
            } else {
                format_bandwidth(stats.read_bytes, duration)
            };

            let write_bytes = stats
                .write_bytes
                .saturating_sub(stats.cancelled_write_bytes);
            let write_str = if accumulated {
                human_size(write_bytes as i64)
            } else {
                format_bandwidth(write_bytes, duration)
            };

            let mut cmdline = process.get_cmdline();

            if has_delay {
                let io_delay = format_delay_percent(stats.blkio_delay_total, duration);
                let swapin_delay = format_delay_percent(stats.swapin_delay_total, duration);

                let remaining = (width as usize).saturating_sub(65);
                if cmdline.len() > remaining {
                    cmdline.truncate(remaining.saturating_sub(1));
                    cmdline.push('~');
                }

                write!(
                    stdout,
                    "{:>7}  {:>4}  {:<8} {:>11} {:>11}  {:>6}      {:>2} {}",
                    process.tid,
                    process.get_prio(),
                    process.get_user(),
                    read_str,
                    write_str,
                    swapin_delay,
                    io_delay,
                    cmdline
                )?;
            } else {
                let remaining = (width as usize).saturating_sub(65);
                if cmdline.len() > remaining {
                    cmdline.truncate(remaining.saturating_sub(1));
                    cmdline.push('~');
                }

                write!(
                    stdout,
                    "{:>7}  {:>4}  {:<8} {:>11} {:>11} {} {}",
                    process.tid,
                    process.get_prio(),
                    process.get_user(),
                    read_str,
                    write_str,
                    "?unavailable?",
                    cmdline
                )?;
            }
        }

        stdout.flush()?;
        Ok(())
    }
}
