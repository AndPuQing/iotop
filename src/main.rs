mod process;
mod taskstats;
mod ui;

use anyhow::Result;
use clap::Parser;
use process::ProcessList;
use std::thread;
use std::time::Duration;
use taskstats::{TaskStats, TaskStatsConnection};
use ui::UI;

#[derive(Parser, Debug)]
#[command(name = "iotop")]
#[command(about = "A Rust implementation of iotop - display I/O usage of processes", long_about = None)]
struct Args {
    /// Only show processes or threads actually doing I/O
    #[arg(short = 'o', long = "only")]
    only: bool,

    /// Show processes, not all threads
    #[arg(short = 'P', long = "processes")]
    processes: bool,

    /// Show accumulated I/O instead of bandwidth
    #[arg(short = 'a', long = "accumulated")]
    accumulated: bool,

    /// Delay between iterations in seconds
    #[arg(short = 'd', long = "delay", default_value = "1.0")]
    delay: f64,

    /// Number of iterations before ending (infinite if not specified)
    #[arg(short = 'n', long = "iter")]
    iterations: Option<usize>,

    /// Batch mode (non-interactive)
    #[arg(short = 'b', long = "batch")]
    batch: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Check for requirements
    check_requirements()?;

    // Connect to taskstats
    let taskstats_conn = TaskStatsConnection::new()?;
    let mut process_list = ProcessList::new(taskstats_conn);

    if args.batch {
        run_batch_mode(&mut process_list, &args)?;
    } else {
        run_interactive_mode(&mut process_list, &args)?;
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

fn run_interactive_mode(process_list: &mut ProcessList, args: &Args) -> Result<()> {
    let mut ui = UI::new()?;
    let mut iteration = 0;

    loop {
        // Refresh process data
        let (total, actual) = process_list.refresh_processes(args.processes)?;

        // Get mutable references to processes
        let mut processes: Vec<&process::ProcessInfo> = process_list.processes.values().collect();

        // Render UI
        ui.render(&mut processes, total, actual, process_list.duration)?;

        // Handle input
        if ui.handle_input()? {
            break; // User requested quit
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

    ui.cleanup()?;
    Ok(())
}

fn run_batch_mode(process_list: &mut ProcessList, args: &Args) -> Result<()> {
    use std::io::{self, Write};

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
            } else {
                if writeln!(
                    io::stdout(),
                    "{:>7}  {:>4}  {:<8}     {:>10}  {:>11} {} COMMAND",
                    "TID",
                    "PRIO",
                    "USER",
                    "DISK READ",
                    "DISK WRITE",
                    "?unavailable?"
                )
                .is_err()
                {
                    return Ok(());
                }
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
            } else {
                if writeln!(
                    io::stdout(),
                    "{:>7}  {:>4}  {:<8} {:>11} {:>11} {} {}",
                    process.tid,
                    process.get_prio(),
                    process.get_user(),
                    read_str,
                    write_str,
                    "?unavailable?",
                    process.get_cmdline()
                )
                .is_err()
                {
                    return Ok(());
                }
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
