mod ioprio;
mod proc_reader;
mod process;
mod taskstats;
mod ui;

use anyhow::Result;
use argh::FromArgs;
use crossterm::event::MouseEventKind;
use crossterm::event::{KeyCode, KeyModifiers};
use nix::unistd::User;
use process::{ProcessInfo, ProcessList, ProcessSnapshot};
use regex::Regex;
use taskstats::{TaskStats, TaskStatsConnection};
use tokio_util::sync::CancellationToken;
use ui::{Event, SortColumn, Tui, UIState};

// UI scroll constants
const SCROLL_PAGE_SIZE: usize = 10;
const SCROLL_WHEEL_SIZE: usize = 3;
const UI_HEADER_HEIGHT: u16 = 7;

/// A Rust implementation of iotop - display I/O usage of processes
#[derive(FromArgs, Debug)]
struct Args {
    /// only show processes or threads actually doing I/O
    #[argh(switch, short = 'o')]
    only: bool,

    /// show processes, not all threads
    #[argh(switch, short = 'P')]
    processes: bool,

    /// show accumulated I/O instead of bandwidth
    #[argh(switch, short = 'a')]
    accumulated: bool,

    /// delay between iterations in seconds
    #[argh(option, short = 'd', default = "1.0")]
    delay: f64,

    /// number of iterations before ending (infinite if not specified)
    #[argh(option, short = 'n')]
    iterations: Option<usize>,

    /// batch mode (non-interactive)
    #[argh(switch, short = 'b')]
    batch: bool,

    /// processes/threads to monitor (can be repeated)
    #[argh(option, short = 'p')]
    pid: Vec<i32>,

    /// users to monitor (username or UID, can be repeated)
    #[argh(option, short = 'u')]
    user: Vec<String>,

    /// regex to match against the command line (can be repeated)
    #[argh(option, short = 'c')]
    command: Vec<String>,

    /// add timestamp on each line (implies --batch)
    #[argh(switch, short = 't')]
    time: bool,

    /// suppress column names and headers (implies --batch)
    #[argh(switch, short = 'q')]
    quiet: bool,

    /// use kilobytes instead of human-friendly units
    #[argh(switch, short = 'k')]
    kilobytes: bool,

    /// output one JSON object per iteration (implies --batch)
    #[argh(switch)]
    json: bool,

    /// output CSV rows (implies --batch)
    #[argh(switch)]
    csv: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();

    // Check for requirements
    check_requirements()?;

    // Resolve usernames to UIDs
    let uids = resolve_users(&args.user)?;

    // Compile command-line regex filters (invalid patterns abort early)
    let command_regexes = compile_command_regexes(&args.command)?;

    // Connect to taskstats
    let taskstats_conn = TaskStatsConnection::new()?;
    warn_if_taskstats_unreadable(&taskstats_conn);
    let mut process_list = ProcessList::new(taskstats_conn)
        .with_pids(args.pid.clone())
        .with_uids(uids.clone());

    if args.batch || args.time || args.quiet || args.json || args.csv {
        run_batch_mode(&mut process_list, &args, &command_regexes)?;
    } else {
        run_interactive_mode(&mut process_list, &args, &command_regexes).await?;
    }

    Ok(())
}

/// Warn (to stderr) when taskstats is not readable with the current privileges,
/// so the user is not misled by silent 0 B/s output. This runs in both batch and
/// interactive modes but never aborts the program. It is a no-op when running as
/// root or with CAP_NET_ADMIN.
fn warn_if_taskstats_unreadable(conn: &taskstats::TaskStatsConnection) {
    match conn.probe_access() {
        taskstats::TaskstatsAccess::Accessible => {}
        taskstats::TaskstatsAccess::PermissionDenied => {
            eprintln!(
                "WARNING: cannot read per-process I/O statistics (needs root or the CAP_NET_ADMIN capability).\n\
                 Per-process bandwidth will show 0 B/s instead of real data.\n\
                 Run with sudo, or grant the capability once:\n\
                   sudo setcap cap_net_admin+ep $(command -v iotop)"
            );
        }
        taskstats::TaskstatsAccess::Unsupported => {
            eprintln!(
                "WARNING: taskstats is unavailable on this system; per-process I/O statistics will show 0 B/s."
            );
        }
    }
}

fn check_requirements() -> Result<()> {
    // Check if /proc/self/io exists (I/O accounting)
    if !std::path::Path::new("/proc/self/io").exists() {
        anyhow::bail!(
            "Could not run iotop as some of the requirements are not met:\n\
             - Linux >= 2.6.20 with I/O accounting support \n\
             (CONFIG_TASKSTATS, CONFIG_TASK_DELAY_ACCT, CONFIG_TASK_IO_ACCOUNTING, \n\
             kernel.task_delayacct sysctl)"
        );
    }

    // Check if /proc/vmstat exists (VM event counters)
    if !std::path::Path::new("/proc/vmstat").exists() {
        anyhow::bail!(
            "Could not run iotop as some of the requirements are not met:\n\
             - Linux kernel with VM event counters (CONFIG_VM_EVENT_COUNTERS)"
        );
    }

    Ok(())
}

fn resolve_users(users: &[String]) -> Result<Vec<u32>> {
    let mut uids = Vec::new();

    for user_str in users {
        // Try parsing as UID first
        if let Ok(uid) = user_str.parse::<u32>() {
            uids.push(uid);
        } else {
            // Try resolving as username
            match User::from_name(user_str)? {
                Some(user) => uids.push(user.uid.as_raw()),
                None => {
                    anyhow::bail!("Unknown user: {}", user_str);
                }
            }
        }
    }

    Ok(uids)
}

/// Compile the `-c/--command` regex filters. Returns an error describing the
/// offending pattern so invalid regexes fail fast at startup.
fn compile_command_regexes(patterns: &[String]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|p| {
            Regex::new(p).map_err(|e| anyhow::anyhow!("invalid command regex {:?}: {}", p, e))
        })
        .collect()
}

/// Filter a process list in place so only processes whose command line matches
/// at least one of the given regexes remain. When `regexes` is empty no
/// filtering is applied. Used by batch and interactive modes alike.
fn filter_by_command(processes: &mut Vec<&ProcessInfo>, regexes: &[Regex]) {
    if regexes.is_empty() {
        return;
    }
    processes.retain(|p| regexes.iter().any(|re| re.is_match(p.get_cmdline())));
}

async fn run_interactive_mode(
    process_list: &mut ProcessList,
    args: &Args,
    command_regexes: &[Regex],
) -> Result<()> {
    let mut tui = Tui::new()?;
    tui.enter()?;

    let mut state = UIState::default();
    let mut iteration = 0;
    let has_delay_acct = TaskStats::has_delay_acct();

    // Apply command line arguments to initial state
    state.only_active = args.only;
    state.accumulated = args.accumulated;
    state.show_processes = args.processes;
    state.kilobytes = args.kilobytes;

    // Start async data stream
    let mut data_cancel_token = CancellationToken::new();
    let mut data_stream = ProcessList::spawn_refresh_stream(
        1.0 / args.delay,
        state.show_processes,
        process_list.taskstats_conn.clone(),
        args.pid.clone(),
        process_list.uids.clone(),
        data_cancel_token.clone(),
    );

    // Store current snapshot
    let mut current_snapshot: Option<ProcessSnapshot> = None;

    loop {
        // Wait for next event
        tokio::select! {
            // Handle data updates from the stream
            Some(snapshot) = data_stream.recv() => {
                current_snapshot = Some(snapshot.clone());
                // Send event to TUI event loop if not paused
                if !state.paused {
                    let _ = tui.event_tx.send(Event::DataUpdate(snapshot));
                }
            }
            // Handle UI events
            Some(event) = tui.next_event() => {
                match event {
                    Event::Init => {

                    }
                    Event::DataUpdate(snapshot) => {
                        render_snapshot(
                            &mut tui,
                            &snapshot,
                            &mut state,
                            has_delay_acct,
                            command_regexes,
                        )?;

                        // Check iteration limit
                        if let Some(max_iter) = args.iterations {
                            iteration += 1;
                            if iteration >= max_iter {
                                break;
                            }
                        }
                    }
                    Event::Render => {
                        if let Some(ref snapshot) = current_snapshot {
                            render_snapshot(
                                &mut tui,
                                snapshot,
                                &mut state,
                                has_delay_acct,
                                command_regexes,
                            )?;
                        }
                    }
                    Event::Key(key) => match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('o') | KeyCode::Char('O') => {
                            state.only_active = !state.only_active;
                            state.scroll_offset = 0;
                            state.selection_mode = false;
                            state.selected_row = None;
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            state.accumulated = !state.accumulated;
                            state.scroll_offset = 0;
                            state.selection_mode = false;
                            state.selected_row = None;
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            state.sort_reverse = !state.sort_reverse;
                            state.scroll_offset = 0;
                            state.selection_mode = false;
                            state.selected_row = None;
                        }
                        KeyCode::Char('k') | KeyCode::Char('K') => {
                            state.kilobytes = !state.kilobytes;
                        }
                        KeyCode::Char('c') | KeyCode::Char('C') => {
                            state.column_mode = !state.column_mode;
                            if state.column_mode {
                                // Start the cursor on the current sort column.
                                state.column_cursor = state.sort_column;
                            }
                        }
                        KeyCode::Char(' ') => {
                            if state.column_mode {
                                // Toggle the selected column; never hide the last one.
                                if state.visible_columns.is_visible(state.column_cursor) {
                                    if state.visible_columns.count(has_delay_acct) > 1 {
                                        state
                                            .visible_columns
                                            .set_visible(state.column_cursor, false);
                                    }
                                } else {
                                    state
                                        .visible_columns
                                        .set_visible(state.column_cursor, true);
                                }
                            } else {
                                state.paused = !state.paused;
                            }
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            state.show_processes = !state.show_processes;
                            state.scroll_offset = 0;
                            state.selection_mode = false;
                            state.selected_row = None;

                            data_cancel_token.cancel();
                            data_cancel_token = CancellationToken::new();
                            data_stream = ProcessList::spawn_refresh_stream(
                                1.0 / args.delay,
                                state.show_processes,
                                process_list.taskstats_conn.clone(),
                                args.pid.clone(),
                                process_list.uids.clone(),
                                data_cancel_token.clone(),
                            );
                        }
                        KeyCode::Left => {
                            if state.column_mode {
                                state.column_cursor =
                                    state.column_cursor.cycle_backward(has_delay_acct);
                            } else {
                                state.sort_column =
                                    state.sort_column.cycle_backward(has_delay_acct);
                                state.scroll_offset = 0;
                                state.selection_mode = false;
                                state.selected_row = None;
                            }
                        }
                        KeyCode::Right => {
                            if state.column_mode {
                                state.column_cursor =
                                    state.column_cursor.cycle_forward(has_delay_acct);
                            } else {
                                state.sort_column =
                                    state.sort_column.cycle_forward(has_delay_acct);
                                state.scroll_offset = 0;
                                state.selection_mode = false;
                                state.selected_row = None;
                            }
                        }
                        KeyCode::Up => {
                            if !state.selection_mode {
                                state.selection_mode = true;
                                state.selected_row = Some(0);
                            } else if let Some(selected) = state.selected_row {
                                state.selected_row = Some(selected.saturating_sub(1));
                                // Adjust scroll_offset if selected row is above visible area
                                if state.selected_row.unwrap() < state.scroll_offset {
                                    state.scroll_offset = state.selected_row.unwrap();
                                }
                            }
                        }
                        KeyCode::Down => {
                            if !state.selection_mode {
                                state.selection_mode = true;
                                state.selected_row = Some(0);
                            } else if let Some(selected) = state.selected_row {
                                state.selected_row = Some(selected.saturating_add(1));
                            }
                        }
                        KeyCode::Home => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                if state.selection_mode {
                                    state.selected_row = Some(0);
                                }
                                state.scroll_offset = 0;
                            } else {
                                state.sort_column = SortColumn::available_columns(has_delay_acct)[0];
                                state.selection_mode = false;
                                state.selected_row = None;
                            }
                        }
                        KeyCode::End => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                state.scroll_offset = usize::MAX;
                                if state.selection_mode {
                                    state.selected_row = Some(usize::MAX);
                                }
                            } else {
                                let columns = SortColumn::available_columns(has_delay_acct);
                                state.sort_column = columns[columns.len() - 1];
                                state.selection_mode = false;
                                state.selected_row = None;
                            }
                        }
                        KeyCode::PageUp => {
                            if state.selection_mode {
                                if let Some(selected) = state.selected_row {
                                    state.selected_row = Some(selected.saturating_sub(SCROLL_PAGE_SIZE));
                                    if let Some(sel) = state.selected_row {
                                        if sel < state.scroll_offset {
                                            state.scroll_offset = sel;
                                        }
                                    }
                                }
                            } else {
                                state.scroll_offset = state.scroll_offset.saturating_sub(SCROLL_PAGE_SIZE);
                            }
                        }
                        KeyCode::PageDown => {
                            if state.selection_mode {
                                if let Some(selected) = state.selected_row {
                                    state.selected_row = Some(selected.saturating_add(SCROLL_PAGE_SIZE));
                                }
                            } else {
                                state.scroll_offset = state.scroll_offset.saturating_add(SCROLL_PAGE_SIZE);
                            }
                        }
                        KeyCode::Esc => {
                            state.column_mode = false;
                            state.selection_mode = false;
                            state.selected_row = None;
                        }
                        _ => {}
                    },
                    Event::Mouse(mouse) => {

                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                if state.selection_mode {
                                    if let Some(selected) = state.selected_row {
                                        state.selected_row = Some(selected.saturating_sub(SCROLL_WHEEL_SIZE));
                                        if let Some(sel) = state.selected_row {
                                            if sel < state.scroll_offset {
                                                state.scroll_offset = sel;
                                            }
                                        }
                                    }
                                } else {
                                    state.scroll_offset = state.scroll_offset.saturating_sub(SCROLL_WHEEL_SIZE);
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                if state.selection_mode {
                                    if let Some(selected) = state.selected_row {
                                        state.selected_row = Some(selected.saturating_add(SCROLL_WHEEL_SIZE));
                                    }
                                } else {
                                    state.scroll_offset = state.scroll_offset.saturating_add(SCROLL_WHEEL_SIZE);
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::Resize(_, _) => {
                        // Terminal was resized, redraw on next render
                    }
                    Event::Error => {
                        // Handle error event
                        break;
                    }
                    Event::Quit => {
                        break;
                    }
                    _ => {}
                }
            }
            else => {
                // Both channels closed
                break;
            }
        }
    }

    // Stop data stream
    data_cancel_token.cancel();

    // Ensure terminal cleanup happens
    tui.exit()?;

    Ok(())
}

/// Prepare and render a snapshot of process data to the TUI
fn render_snapshot(
    tui: &mut Tui,
    snapshot: &ProcessSnapshot,
    state: &mut UIState,
    has_delay_acct: bool,
    command_regexes: &[Regex],
) -> Result<()> {
    let mut processes: Vec<&process::ProcessInfo> = snapshot.processes.values().collect();

    if state.only_active {
        processes.retain(|p| p.did_some_io(state.accumulated));
    }
    filter_by_command(&mut processes, command_regexes);

    sort_processes(&mut processes, state);

    let available_height = tui
        .terminal
        .size()
        .map(|size| size.height.saturating_sub(UI_HEADER_HEIGHT) as usize)
        .unwrap_or(10);

    // Clamp selected_row to valid range if in selection mode
    if state.selection_mode {
        if let Some(selected) = state.selected_row {
            let max_row = processes.len().saturating_sub(1);
            state.selected_row = Some(selected.min(max_row));

            // Auto-scroll to keep selected row visible
            if let Some(selected) = state.selected_row {
                if selected < state.scroll_offset {
                    state.scroll_offset = selected;
                } else if selected >= state.scroll_offset + available_height {
                    state.scroll_offset =
                        selected.saturating_sub(available_height.saturating_sub(1));
                }
            }
        }
    }

    // Draw the UI
    tui.draw(
        &processes,
        snapshot.total_io,
        snapshot.actual_io,
        snapshot.duration,
        state,
        has_delay_acct,
    )?;

    Ok(())
}

fn sort_processes(processes: &mut Vec<&process::ProcessInfo>, state: &UIState) {
    processes.sort_by(|a, b| {
        let stats_a = if state.accumulated {
            &a.stats_accum
        } else {
            &a.stats_delta
        };
        let stats_b = if state.accumulated {
            &b.stats_accum
        } else {
            &b.stats_delta
        };

        let ordering = match state.sort_column {
            SortColumn::Pid => a.tid.cmp(&b.tid),
            SortColumn::Prio => a.get_prio().cmp(b.get_prio()),
            SortColumn::User => a.get_user().cmp(b.get_user()),
            SortColumn::Read => stats_b.read_bytes.cmp(&stats_a.read_bytes),
            SortColumn::Write => {
                let write_a = stats_a
                    .write_bytes
                    .saturating_sub(stats_a.cancelled_write_bytes);
                let write_b = stats_b
                    .write_bytes
                    .saturating_sub(stats_b.cancelled_write_bytes);
                write_b.cmp(&write_a)
            }
            SortColumn::Swapin => stats_b.swapin_delay_total.cmp(&stats_a.swapin_delay_total),
            SortColumn::Io => stats_b.blkio_delay_total.cmp(&stats_a.blkio_delay_total),

            SortColumn::Command => a.get_cmdline().cmp(b.get_cmdline()),
        };

        if state.sort_reverse {
            ordering
                .then_with(|| a.pid.cmp(&b.pid))
                .then_with(|| a.tid.cmp(&b.tid))
        } else {
            ordering
                .reverse()
                .then_with(|| a.pid.cmp(&b.pid))
                .then_with(|| a.tid.cmp(&b.tid))
        }
    });
}

/// Run iotop in batch mode (non-interactive)
///
/// Batch mode outputs process I/O statistics to stdout in a parseable format.
/// This function gracefully handles broken pipe errors (e.g., when output is
/// piped to `head` or similar utilities) by returning Ok(()) when write errors occur.
fn run_batch_mode(
    process_list: &mut ProcessList,
    args: &Args,
    command_regexes: &[Regex],
) -> Result<()> {
    // Machine-readable output modes take precedence over the plain-text batch
    // output. They share the same refresh loop and iteration/delay semantics.
    if args.json {
        return run_json_mode(process_list, args, command_regexes);
    }
    if args.csv {
        return run_csv_mode(process_list, args, command_regexes);
    }

    use std::io::{self, Write};
    use std::thread;
    use std::time::Duration;

    let mut iteration = 0;

    loop {
        let timestamp = if args.time {
            chrono::Local::now().format("%H:%M:%S ").to_string()
        } else {
            String::new()
        };

        let (total, actual) = process_list.refresh_processes(args.processes)?;

        if !args.quiet {
            if writeln!(
                io::stdout(),
                "{}Total DISK READ :   {:>14} | Total DISK WRITE :   {:>14}",
                timestamp,
                ui::format_bandwidth(total.0, process_list.duration),
                ui::format_bandwidth(total.1, process_list.duration)
            )
            .is_err()
            {
                return Ok(());
            }

            if writeln!(
                io::stdout(),
                "{}Actual DISK READ:   {:>14} | Actual DISK WRITE:   {:>14}",
                timestamp,
                ui::format_bandwidth(actual.0, process_list.duration),
                ui::format_bandwidth(actual.1, process_list.duration)
            )
            .is_err()
            {
                return Ok(());
            }
        }

        if iteration == 0 && !args.quiet {
            let has_delay = TaskStats::has_delay_acct();
            let header_prefix = if args.time { "    TIME " } else { "" };
            if has_delay {
                if writeln!(
                    io::stdout(),
                    "{}{:>7}  {:>4}  {:<8}     {:>10}  {:>11}  {:>6}      {:>2}    COMMAND",
                    header_prefix,
                    "TID",
                    "PRIO",
                    "USER",
                    "DISK READ",
                    "DISK WRITE",
                    "SWAPIN",
                    "IO"
                )
                .is_err()
                {
                    return Ok(());
                }
            } else if writeln!(
                io::stdout(),
                "{}{:>7}  {:>4}  {:<8}     {:>10}  {:>11} ?unavailable? COMMAND",
                header_prefix,
                "TID",
                "PRIO",
                "USER",
                "DISK READ",
                "DISK WRITE"
            )
            .is_err()
            {
                return Ok(());
            }
        }

        let mut processes: Vec<&process::ProcessInfo> = process_list.processes.values().collect();

        if args.only {
            processes.retain(|p| p.did_some_io(args.accumulated));
        }
        filter_by_command(&mut processes, command_regexes);

        processes.sort_by(|a, b| {
            let stats_a = if args.accumulated {
                &a.stats_accum
            } else {
                &a.stats_delta
            };
            let stats_b = if args.accumulated {
                &b.stats_accum
            } else {
                &b.stats_delta
            };
            stats_b
                .blkio_delay_total
                .cmp(&stats_a.blkio_delay_total)
                .then_with(|| a.pid.cmp(&b.pid))
                .then_with(|| a.tid.cmp(&b.tid))
        });

        for process in processes {
            let stats = if args.accumulated {
                &process.stats_accum
            } else {
                &process.stats_delta
            };

            let read_str = if args.kilobytes {
                if args.accumulated {
                    ui::format_size_kb(stats.read_bytes)
                } else {
                    ui::format_bandwidth_kb(stats.read_bytes, process_list.duration)
                }
            } else if args.accumulated {
                ui::human_size(stats.read_bytes as i64)
            } else {
                ui::format_bandwidth(stats.read_bytes, process_list.duration)
            };

            let write_bytes = stats
                .write_bytes
                .saturating_sub(stats.cancelled_write_bytes);
            let write_str = if args.kilobytes {
                if args.accumulated {
                    ui::format_size_kb(write_bytes)
                } else {
                    ui::format_bandwidth_kb(write_bytes, process_list.duration)
                }
            } else if args.accumulated {
                ui::human_size(write_bytes as i64)
            } else {
                ui::format_bandwidth(write_bytes, process_list.duration)
            };

            let has_delay = TaskStats::has_delay_acct();

            if has_delay {
                let io_delay =
                    ui::format_delay_percent(stats.blkio_delay_total, process_list.duration);
                let swapin_delay =
                    ui::format_delay_percent(stats.swapin_delay_total, process_list.duration);

                if writeln!(
                    io::stdout(),
                    "{}{:>7}  {:>4}  {:<8} {:>11} {:>11}  {:>6}      {:>2} {}",
                    timestamp,
                    process.tid,
                    process.get_prio(),
                    process.get_user(),
                    read_str,
                    write_str,
                    swapin_delay,
                    io_delay,
                    process.get_cmdline()
                )
                .is_err()
                {
                    return Ok(());
                }
            } else if writeln!(
                io::stdout(),
                "{}{:>7}  {:>4}  {:<8} {:>11} {:>11} ?unavailable? {}",
                timestamp,
                process.tid,
                process.get_prio(),
                process.get_user(),
                read_str,
                write_str,
                process.get_cmdline()
            )
            .is_err()
            {
                return Ok(());
            }
        }

        if let Some(max_iter) = args.iterations {
            iteration += 1;
            if iteration >= max_iter {
                break;
            }
        }

        thread::sleep(Duration::from_secs_f64(args.delay));
    }
    Ok(())
}

/// Percentage of wall-clock time spent waiting on a delay, as a bare number
/// (e.g. 3.5 means 3.5%). Used by the machine-readable output modes.
fn delay_percent(delay_ns: u64, duration: f64) -> f64 {
    if duration <= 0.0 {
        0.0
    } else {
        (delay_ns as f64 / (duration * 1_000_000_000.0)) * 100.0
    }
}

/// Sort a process list by the same rule used by the plain-text batch output:
/// descending block-I/O delay, then PID, then TID for a stable order.
fn sort_processes_by_io(processes: &mut Vec<&process::ProcessInfo>, args: &Args) {
    processes.sort_by(|a, b| {
        let stats_a = if args.accumulated {
            &a.stats_accum
        } else {
            &a.stats_delta
        };
        let stats_b = if args.accumulated {
            &b.stats_accum
        } else {
            &b.stats_delta
        };
        stats_b
            .blkio_delay_total
            .cmp(&stats_a.blkio_delay_total)
            .then_with(|| a.pid.cmp(&b.pid))
            .then_with(|| a.tid.cmp(&b.tid))
    });
}

/// Per-process values (read/write bytes and delay percentages) for a given
/// bandwidth/accumulated mode, shared by the JSON and CSV outputs.
struct ProcessRow {
    tid: i32,
    pid: i32,
    prio: String,
    user: String,
    read: u64,
    write: u64,
    swapin: f64,
    io: f64,
    command: String,
}

fn collect_row(process: &process::ProcessInfo, accumulated: bool, duration: f64) -> ProcessRow {
    let stats = if accumulated {
        &process.stats_accum
    } else {
        &process.stats_delta
    };
    let read = stats.read_bytes;
    let write = stats
        .write_bytes
        .saturating_sub(stats.cancelled_write_bytes);

    ProcessRow {
        tid: process.tid,
        pid: process.pid,
        prio: process.get_prio().to_string(),
        user: process.get_user().to_string(),
        read,
        write,
        swapin: delay_percent(stats.swapin_delay_total, duration),
        io: delay_percent(stats.blkio_delay_total, duration),
        command: process.get_cmdline().to_string(),
    }
}

/// Batch mode with `--json`: emit one JSON object per iteration, containing the
/// iteration timestamp, total/actual disk I/O, and the process list.
fn run_json_mode(
    process_list: &mut ProcessList,
    args: &Args,
    command_regexes: &[Regex],
) -> Result<()> {
    use std::io::{self, Write};
    use std::thread;
    use std::time::Duration;

    let mut iteration = 0;

    loop {
        let timestamp = if args.time {
            chrono::Local::now().format("%H:%M:%S").to_string()
        } else {
            String::new()
        };

        let (total, actual) = process_list.refresh_processes(args.processes)?;

        let mut processes: Vec<&process::ProcessInfo> = process_list.processes.values().collect();
        if args.only {
            processes.retain(|p| p.did_some_io(args.accumulated));
        }
        filter_by_command(&mut processes, command_regexes);
        sort_processes_by_io(&mut processes, args);

        let rows: Vec<serde_json::Value> = processes
            .iter()
            .map(|p| {
                let row = collect_row(p, args.accumulated, process_list.duration);
                serde_json::json!({
                    "tid": row.tid,
                    "pid": row.pid,
                    "prio": row.prio,
                    "user": row.user,
                    "read": row.read,
                    "write": row.write,
                    "swapin": row.swapin,
                    "io": row.io,
                    "command": row.command,
                })
            })
            .collect();

        let obj = serde_json::json!({
            "timestamp": timestamp,
            "total_read": total.0,
            "total_write": total.1,
            "actual_read": actual.0,
            "actual_write": actual.1,
            "processes": rows,
        });

        if writeln!(io::stdout(), "{}", serde_json::to_string(&obj)?).is_err() {
            return Ok(());
        }

        if let Some(max_iter) = args.iterations {
            iteration += 1;
            if iteration >= max_iter {
                break;
            }
        }

        thread::sleep(Duration::from_secs_f64(args.delay));
    }
    Ok(())
}

/// Escape a single CSV field: quote it when it contains a comma, quote, or
/// newline, and double any embedded quotes.
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Batch mode with `--csv`: emit one CSV row per process per iteration. A header
/// row is printed before the first iteration (unless `--quiet`). With `--time` a
/// leading time column is added. Compatible with `-t`/`-q`.
fn run_csv_mode(
    process_list: &mut ProcessList,
    args: &Args,
    command_regexes: &[Regex],
) -> Result<()> {
    use std::io::{self, Write};
    use std::thread;
    use std::time::Duration;

    let mut iteration = 0;
    let mut header_printed = false;

    loop {
        let timestamp = if args.time {
            chrono::Local::now().format("%H:%M:%S").to_string()
        } else {
            String::new()
        };

        let _ = process_list.refresh_processes(args.processes)?;

        let has_delay = TaskStats::has_delay_acct();

        let mut processes: Vec<&process::ProcessInfo> = process_list.processes.values().collect();
        if args.only {
            processes.retain(|p| p.did_some_io(args.accumulated));
        }
        filter_by_command(&mut processes, command_regexes);
        sort_processes_by_io(&mut processes, args);

        if !args.quiet && !header_printed {
            let mut header = Vec::new();
            if args.time {
                header.push("time".to_string());
            }
            header.extend([
                "tid".to_string(),
                "pid".to_string(),
                "prio".to_string(),
                "user".to_string(),
                "read".to_string(),
                "write".to_string(),
                "swapin".to_string(),
                "io".to_string(),
                "command".to_string(),
            ]);
            if writeln!(io::stdout(), "{}", header.join(",")).is_err() {
                return Ok(());
            }
            header_printed = true;
        }

        for process in &processes {
            let row = collect_row(process, args.accumulated, process_list.duration);

            let mut fields = Vec::new();
            if args.time {
                fields.push(csv_field(&timestamp));
            }
            fields.push(format!("{}", row.tid));
            fields.push(format!("{}", row.pid));
            fields.push(csv_field(&row.prio));
            fields.push(csv_field(&row.user));
            fields.push(format!("{}", row.read));
            fields.push(format!("{}", row.write));
            fields.push(if has_delay {
                format!("{:.2}", row.swapin)
            } else {
                String::new()
            });
            fields.push(if has_delay {
                format!("{:.2}", row.io)
            } else {
                String::new()
            });
            fields.push(csv_field(&row.command));

            if writeln!(io::stdout(), "{}", fields.join(",")).is_err() {
                return Ok(());
            }
        }

        if let Some(max_iter) = args.iterations {
            iteration += 1;
            if iteration >= max_iter {
                break;
            }
        }

        thread::sleep(Duration::from_secs_f64(args.delay));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_with_cmdline(pid: i32, cmdline: &str) -> ProcessInfo {
        let mut p = ProcessInfo::new(pid);
        p.cmdline = Some(cmdline.to_string());
        p
    }

    fn refs(owned: &mut [ProcessInfo]) -> Vec<&ProcessInfo> {
        owned.iter().collect()
    }

    #[test]
    fn test_compile_command_regexes_valid() {
        let regexes = compile_command_regexes(&["bash".to_string(), "ssh.*".to_string()]).unwrap();
        assert_eq!(regexes.len(), 2);
    }

    #[test]
    fn test_compile_command_regexes_invalid() {
        assert!(compile_command_regexes(&["[".to_string()]).is_err());
    }

    #[test]
    fn test_filter_by_command_empty_noop() {
        let mut owned = vec![
            process_with_cmdline(1, "bash"),
            process_with_cmdline(2, "sshd -D"),
        ];
        let mut processes = refs(&mut owned);
        filter_by_command(&mut processes, &[]);
        assert_eq!(processes.len(), 2);
    }

    #[test]
    fn test_filter_by_command_matches_substring() {
        let mut owned = vec![
            process_with_cmdline(1, "/usr/bin/bash --login"),
            process_with_cmdline(2, "sshd: root@pts/0"),
            process_with_cmdline(3, "nginx: worker"),
        ];
        let regexes = compile_command_regexes(&["bash".to_string()]).unwrap();
        let mut processes = refs(&mut owned);
        filter_by_command(&mut processes, &regexes);
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 1);
    }

    #[test]
    fn test_filter_by_command_multiple_regexes_any() {
        let mut owned = vec![
            process_with_cmdline(1, "bash"),
            process_with_cmdline(2, "nginx"),
            process_with_cmdline(3, "sleep 10"),
        ];
        let regexes =
            compile_command_regexes(&["bash".to_string(), "nginx".to_string()]).unwrap();
        let mut processes = refs(&mut owned);
        filter_by_command(&mut processes, &regexes);
        let remaining: Vec<i32> = processes.iter().map(|p| p.pid).collect();
        assert_eq!(remaining, vec![1, 2]);
    }

    #[test]
    fn test_filter_by_command_regex_anchoring() {
        let mut owned = vec![
            process_with_cmdline(1, "bash"),
            process_with_cmdline(2, "notbash"),
        ];
        let regexes = compile_command_regexes(&["^bash$".to_string()]).unwrap();
        let mut processes = refs(&mut owned);
        filter_by_command(&mut processes, &regexes);
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 1);
    }
}
