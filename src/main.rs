mod process;
mod taskstats;
mod ui;

use anyhow::Result;
use argh::FromArgs;
use crossterm::event::{KeyCode, KeyModifiers};
use process::{ProcessList, ProcessSnapshot};
use taskstats::{TaskStats, TaskStatsConnection};
use tokio_util::sync::CancellationToken;
use ui::{Event, SortColumn, Tui, UIState};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();

    // Check for requirements
    check_requirements()?;

    // Connect to taskstats
    let taskstats_conn = TaskStatsConnection::new()?;
    let mut process_list = ProcessList::new(taskstats_conn);

    if args.batch {
        run_batch_mode(&mut process_list, &args)?;
    } else {
        run_interactive_mode(&mut process_list, &args).await?;
    }

    Ok(())
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

async fn run_interactive_mode(process_list: &mut ProcessList, args: &Args) -> Result<()> {
    let mut tui = Tui::new()?;
    tui.enter()?;

    let mut state = UIState::default();
    let mut iteration = 0;
    let has_delay_acct = TaskStats::has_delay_acct();

    // Apply command line arguments to initial state
    state.only_active = args.only;
    state.accumulated = args.accumulated;

    // Start async data stream
    let data_cancel_token = CancellationToken::new();
    let mut data_stream = ProcessList::spawn_refresh_stream(
        1.0 / args.delay,
        args.processes,
        process_list.taskstats_conn.clone(),
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
                        // Initial render - wait for first data update
                    }
                    Event::DataUpdate(snapshot) => {
                        // New data available - render it
                        let mut processes: Vec<&process::ProcessInfo> =
                            snapshot.processes.values().collect();

                        // Filter if only active requested
                        if state.only_active {
                            processes.retain(|p| p.did_some_io(state.accumulated));
                        }

                        // Sort processes based on current sort column
                        sort_processes(&mut processes, &state);

                        // Draw the UI
                        tui.draw(
                            &processes,
                            snapshot.total_io,
                            snapshot.actual_io,
                            snapshot.duration,
                            &state,
                            has_delay_acct,
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
                        // Render event - redraw with current data
                        if let Some(ref snapshot) = current_snapshot {
                            let mut processes: Vec<&process::ProcessInfo> =
                                snapshot.processes.values().collect();

                            if state.only_active {
                                processes.retain(|p| p.did_some_io(state.accumulated));
                            }

                            sort_processes(&mut processes, &state);

                            tui.draw(
                                &processes,
                                snapshot.total_io,
                                snapshot.actual_io,
                                snapshot.duration,
                                &state,
                                has_delay_acct,
                            )?;
                        }
                    }
                    Event::Key(key) => match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('o') | KeyCode::Char('O') => {
                            state.only_active = !state.only_active;
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            state.accumulated = !state.accumulated;
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            state.sort_reverse = !state.sort_reverse;
                        }
                        KeyCode::Char(' ') => {
                            state.paused = !state.paused;
                        }
                        KeyCode::Left => {
                            state.sort_column = state.sort_column.cycle_backward(has_delay_acct);
                        }
                        KeyCode::Right => {
                            state.sort_column = state.sort_column.cycle_forward(has_delay_acct);
                        }
                        KeyCode::Up | KeyCode::Down => {
                            state.sort_reverse = !state.sort_reverse;
                        }
                        _ => {}
                    },
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
            SortColumn::Prio => a.get_prio().cmp(&b.get_prio()),
            SortColumn::User => a.get_user().cmp(&b.get_user()),
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

            SortColumn::Command => a.get_cmdline().cmp(&b.get_cmdline()),
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

fn run_batch_mode(process_list: &mut ProcessList, args: &Args) -> Result<()> {
    use std::io::{self, Write};
    use std::thread;
    use std::time::Duration;

    let mut iteration = 0;

    loop {
        // Refresh process data
        let (total, actual) = process_list.refresh_processes(args.processes)?;

        // Print summary - handle broken pipe
        if writeln!(
            io::stdout(),
            "Total DISK READ :   {:>14} | Total DISK WRITE :   {:>14}",
            ui::format_bandwidth(total.0, process_list.duration),
            ui::format_bandwidth(total.1, process_list.duration)
        )
        .is_err()
        {
            return Ok(());
        }

        if writeln!(
            io::stdout(),
            "Actual DISK READ:   {:>14} | Actual DISK WRITE:   {:>14}",
            ui::format_bandwidth(actual.0, process_list.duration),
            ui::format_bandwidth(actual.1, process_list.duration)
        )
        .is_err()
        {
            return Ok(());
        }

        // Print header on first iteration
        if iteration == 0 {
            let has_delay = TaskStats::has_delay_acct();
            if has_delay {
                if writeln!(
                    io::stdout(),
                    "{:>7}  {:>4}  {:<8}     {:>10}  {:>11}  {:>6}      {:>2}    COMMAND",
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
                "{:>7}  {:>4}  {:<8}     {:>10}  {:>11} ?unavailable? COMMAND",
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

        // Print processes
        let mut processes: Vec<&process::ProcessInfo> = process_list.processes.values().collect();

        // Filter if only active requested
        if args.only {
            processes.retain(|p| p.did_some_io(args.accumulated));
        }

        // Sort by I/O (descending), then group by PID, then by TID
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

            let read_str = if args.accumulated {
                ui::human_size(stats.read_bytes as i64)
            } else {
                ui::format_bandwidth(stats.read_bytes, process_list.duration)
            };

            let write_bytes = stats
                .write_bytes
                .saturating_sub(stats.cancelled_write_bytes);
            let write_str = if args.accumulated {
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
                    "{:>7}  {:>4}  {:<8} {:>11} {:>11}  {:>6}      {:>2} {}",
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
                "{:>7}  {:>4}  {:<8} {:>11} {:>11} ?unavailable? {}",
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

        // Check iteration limit
        if let Some(max_iter) = args.iterations {
            iteration += 1;
            if iteration >= max_iter {
                break;
            }
        }

        // Sleep for delay
        thread::sleep(Duration::from_secs_f64(args.delay));
    }

    Ok(())
}
