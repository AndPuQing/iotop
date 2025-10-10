use anyhow::Result;
use nix::unistd::{Uid, User};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

use crate::proc_reader::ProcReader;
use crate::taskstats::{TaskStats, TaskStatsConnection};

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    #[allow(dead_code)]
    pub tid: i32,
    pub stats_total: Option<TaskStats>,
    pub stats_delta: TaskStats,
}

impl ThreadInfo {
    pub fn new(tid: i32) -> Self {
        Self {
            tid,
            stats_total: None,
            stats_delta: TaskStats::default(),
        }
    }

    pub fn update_stats(&mut self, stats: TaskStats) {
        if let Some(ref total) = self.stats_total {
            self.stats_delta = stats.delta(total);
        }
        self.stats_total = Some(stats);
    }
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: i32, // Parent process ID (TGID)
    pub tid: i32, // Thread ID (this specific thread)
    pub uid: Option<u32>,
    pub user: Option<String>,
    pub prio: Option<String>,
    pub cmdline: Option<String>, // Cached cmdline
    pub threads: HashMap<i32, ThreadInfo>,
    pub stats_delta: TaskStats,
    pub stats_accum: TaskStats,
    #[allow(dead_code)]
    pub stats_accum_timestamp: Instant,
    metadata_initialized: bool, // Track if we've loaded metadata once
}

impl ProcessInfo {
    pub fn new(pid: i32) -> Self {
        Self {
            pid,
            tid: pid,
            uid: None,
            user: None,
            prio: None,
            cmdline: None,
            threads: HashMap::new(),
            stats_delta: TaskStats::default(),
            stats_accum: TaskStats::default(),
            stats_accum_timestamp: Instant::now(),
            metadata_initialized: false,
        }
    }

    pub fn get_user(&self) -> &str {
        if let Some(ref user) = self.user {
            return user;
        }

        // Cache miss - shouldn't happen often, return fallback
        "?"
    }

    fn compute_user(&self) -> String {
        let user_str = if let Some(uid) = self.uid {
            User::from_uid(Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| format!("{}", uid))
        } else {
            format!("{}", self.uid.unwrap_or(0))
        };

        // Truncate to 8 characters using byte slicing for ASCII-safe truncation
        if user_str.len() > 8 {
            user_str.chars().take(8).collect()
        } else {
            user_str
        }
    }

    pub fn get_prio(&self) -> &str {
        if let Some(ref prio) = self.prio {
            return prio;
        }

        // Cache miss - shouldn't happen often, return fallback
        "be/4"
    }

    pub fn get_cmdline(&self) -> &str {
        // Return cached value if available
        if let Some(ref cmdline) = self.cmdline {
            return cmdline;
        }

        // Not cached - return fallback
        "{no such process}"
    }

    pub fn did_some_io(&self, accumulated: bool) -> bool {
        if accumulated {
            !self.stats_accum.is_all_zero()
        } else {
            self.threads.values().any(|t| !t.stats_delta.is_all_zero())
        }
    }

    pub fn update_stats(&mut self) -> bool {
        let mut stats_delta = TaskStats::default();
        let num_threads = self.threads.len();

        if num_threads == 0 {
            return false;
        }

        for thread in self.threads.values() {
            stats_delta.accumulate(&thread.stats_delta);
        }

        // Average delay stats
        stats_delta.blkio_delay_total /= num_threads as u64;
        stats_delta.swapin_delay_total /= num_threads as u64;

        self.stats_delta = stats_delta;
        self.stats_accum.accumulate(&self.stats_delta);

        true
    }
}

#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    pub processes: HashMap<i32, ProcessInfo>,
    pub total_io: (u64, u64),
    pub actual_io: (u64, u64),
    pub duration: f64,
}

pub struct ProcessList {
    pub processes: HashMap<i32, ProcessInfo>,
    pub taskstats_conn: Arc<Mutex<TaskStatsConnection>>,
    pub timestamp: Instant,
    pub duration: f64,
    pub prev_pgpgin: Option<u64>,
    pub prev_pgpgout: Option<u64>,
}

impl ProcessList {
    pub fn new(taskstats_conn: TaskStatsConnection) -> Self {
        Self {
            processes: HashMap::new(),
            taskstats_conn: Arc::new(Mutex::new(taskstats_conn)),
            timestamp: Instant::now(),
            duration: 0.0,
            prev_pgpgin: None,
            prev_pgpgout: None,
        }
    }

    pub fn spawn_refresh_stream(
        update_rate: f64,
        show_processes: bool,
        taskstats_conn: Arc<Mutex<TaskStatsConnection>>,
        cancellation_token: CancellationToken,
    ) -> mpsc::UnboundedReceiver<ProcessSnapshot> {
        let (tx, rx) = mpsc::unbounded_channel();

        task::spawn(async move {
            let mut tick_interval = interval(Duration::from_secs_f64(1.0 / update_rate));
            let mut processes: HashMap<i32, ProcessInfo> = HashMap::new();
            let mut timestamp = Instant::now();
            let mut duration = 0.0;
            let mut prev_pgpgin: Option<u64> = None;
            let mut prev_pgpgout: Option<u64> = None;

            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                    _ = tick_interval.tick() => {
                        // Refresh process data in blocking task to avoid blocking async runtime
                        let taskstats_conn_clone = taskstats_conn.clone();
                        let processes_clone = processes.clone();

                        let result = task::spawn_blocking(move || {
                            let mut temp_list = ProcessList {
                                processes: processes_clone,
                                taskstats_conn: taskstats_conn_clone,
                                timestamp,
                                duration,
                                prev_pgpgin,
                                prev_pgpgout,
                            };

                            let io_stats = temp_list.refresh_processes(show_processes)?;
                            Ok::<_, anyhow::Error>((temp_list, io_stats))
                        }).await;

                        match result {
                            Ok(Ok((updated_list, (total_io, actual_io)))) => {
                                // Update our state
                                processes = updated_list.processes;
                                timestamp = updated_list.timestamp;
                                duration = updated_list.duration;
                                prev_pgpgin = updated_list.prev_pgpgin;
                                prev_pgpgout = updated_list.prev_pgpgout;

                                // Send snapshot
                                let snapshot = ProcessSnapshot {
                                    processes: processes.clone(),
                                    total_io,
                                    actual_io,
                                    duration,
                                };

                                if tx.send(snapshot).is_err() {
                                    // Receiver dropped, stop the stream
                                    break;
                                }
                            }
                            Ok(Err(_)) | Err(_) => {
                                // Error refreshing, continue to next iteration
                                continue;
                            }
                        }
                    }
                }
            }
        });

        rx
    }

    fn read_vmstat(&self) -> Result<(u64, u64)> {
        let content = fs::read_to_string("/proc/vmstat")?;
        let mut pgpgin = 0u64;
        let mut pgpgout = 0u64;

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                match parts[0] {
                    "pgpgin" => pgpgin = parts[1].parse().unwrap_or(0),
                    "pgpgout" => pgpgout = parts[1].parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        // Convert from pages to bytes (assuming 4KB pages)
        Ok((pgpgin * 4096, pgpgout * 4096))
    }

    fn update_process_metadata(process: &mut ProcessInfo, pid_for_status: i32) {
        // Only update metadata once when process is first seen
        if process.metadata_initialized {
            return;
        }

        // Use ProcReader with local cache - cache only benefits within this single call
        // (multiple threads reading same parent /proc files)
        let mut reader = ProcReader::new(pid_for_status);
        if let Ok(metadata) = reader.metadata_bundle(process.pid) {
            process.pid = metadata.pid;
            process.tid = metadata.tid;
            process.uid = Some(metadata.uid);
            process.cmdline = Some(metadata.cmdline);
            process.prio = Some(metadata.priority_str);

            // Compute and cache user string from UID
            process.user = Some(process.compute_user());

            process.metadata_initialized = true;
        }
    }

    fn collect_thread_stats(
        thread: &mut ThreadInfo,
        taskstats_conn: &Arc<Mutex<TaskStatsConnection>>,
    ) -> (u64, u64) {
        if let Ok(mut conn) = taskstats_conn.lock() {
            if let Ok(Some(stats)) = conn.get_task_stats(thread.tid) {
                thread.update_stats(stats);
                let delta = &thread.stats_delta;
                return (delta.read_bytes, delta.write_bytes);
            }
        }
        (0, 0)
    }

    pub fn refresh_processes(&mut self, show_processes: bool) -> Result<((u64, u64), (u64, u64))> {
        let new_timestamp = Instant::now();
        self.duration = new_timestamp.duration_since(self.timestamp).as_secs_f64();
        self.timestamp = new_timestamp;

        let mut total_read = 0u64;
        let mut total_write = 0u64;

        // Read vmstat for actual disk I/O
        let (current_pgpgin, current_pgpgout) = self.read_vmstat().unwrap_or((0, 0));
        let actual_read = self
            .prev_pgpgin
            .map_or(0, |prev| current_pgpgin.saturating_sub(prev));
        let actual_write = self
            .prev_pgpgout
            .map_or(0, |prev| current_pgpgout.saturating_sub(prev));
        self.prev_pgpgin = Some(current_pgpgin);
        self.prev_pgpgout = Some(current_pgpgout);

        // When show_processes=true: List TGIDs, aggregate all threads per process
        // When show_processes=false (default): List all TIDs individually
        if show_processes {
            // Process mode (-P flag): Aggregate threads by TGID
            for entry in fs::read_dir("/proc")?.flatten() {
                if let Ok(file_name) = entry.file_name().into_string() {
                    if let Ok(tgid) = file_name.parse::<i32>() {
                        let process = self
                            .processes
                            .entry(tgid)
                            .or_insert_with(|| ProcessInfo::new(tgid));
                        process.tid = tgid;

                        // Get all threads for this process
                        let task_dir = format!("/proc/{}/task", tgid);
                        let tids: Vec<i32> = fs::read_dir(task_dir)
                            .ok()
                            .map(|entries| {
                                entries
                                    .flatten()
                                    .filter_map(|e| {
                                        e.file_name()
                                            .into_string()
                                            .ok()
                                            .and_then(|s| s.parse::<i32>().ok())
                                    })
                                    .collect()
                            })
                            .unwrap_or_else(|| vec![tgid]);

                        for tid in tids {
                            let thread = process
                                .threads
                                .entry(tid)
                                .or_insert_with(|| ThreadInfo::new(tid));

                            let (read, write) =
                                Self::collect_thread_stats(thread, &self.taskstats_conn);
                            total_read += read;
                            total_write += write;
                        }

                        process.update_stats();
                        Self::update_process_metadata(process, tgid);
                    }
                }
            }
        } else {
            // Thread mode (default): Each thread is a separate entry
            for entry in fs::read_dir("/proc")?.flatten() {
                if let Ok(file_name) = entry.file_name().into_string() {
                    if let Ok(tgid) = file_name.parse::<i32>() {
                        // For each TGID, enumerate all its threads
                        let task_dir = format!("/proc/{}/task", tgid);
                        if let Ok(task_entries) = fs::read_dir(task_dir) {
                            for task_entry in task_entries.flatten() {
                                if let Ok(tid_name) = task_entry.file_name().into_string() {
                                    if let Ok(tid) = tid_name.parse::<i32>() {
                                        // Key by TID, not TGID!
                                        let process = self
                                            .processes
                                            .entry(tid)
                                            .or_insert_with(|| ProcessInfo::new(tgid));
                                        process.tid = tid;

                                        // Add just this one thread
                                        let thread = process
                                            .threads
                                            .entry(tid)
                                            .or_insert_with(|| ThreadInfo::new(tid));

                                        let (read, write) = Self::collect_thread_stats(
                                            thread,
                                            &self.taskstats_conn,
                                        );
                                        total_read += read;
                                        total_write += write;

                                        process.update_stats();
                                        Self::update_process_metadata(process, tid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remove processes that no longer exist
        self.processes.retain(|_, p| !p.threads.is_empty());

        Ok(((total_read, total_write), (actual_read, actual_write)))
    }
}
