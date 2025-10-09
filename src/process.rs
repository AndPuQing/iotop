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
        }
    }

    // Helper function to read and parse /proc/[pid]/status once
    fn read_proc_status(pid: i32) -> HashMap<String, String> {
        let mut result = HashMap::new();
        if let Ok(content) = fs::read_to_string(format!("/proc/{}/status", pid)) {
            for line in content.lines() {
                if let Some((key, value)) = line.split_once(':') {
                    result.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
        }
        result
    }

    pub fn get_user(&self) -> String {
        if let Some(ref user) = self.user {
            // Truncate to 8 characters like original iotop
            if user.len() > 8 {
                return user.chars().take(8).collect();
            }
            return user.clone();
        }

        // Cache miss - compute it on the fly
        self.compute_user()
    }

    pub fn update_user(&mut self) {
        if self.user.is_none() {
            self.user = Some(self.compute_user());
        }
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

        // Truncate to 8 characters
        if user_str.len() > 8 {
            user_str.chars().take(8).collect()
        } else {
            user_str
        }
    }

    pub fn get_prio(&self) -> String {
        if let Some(ref prio) = self.prio {
            return prio.clone();
        }

        // Cache miss - compute on the fly
        self.compute_prio()
    }

    pub fn update_prio(&mut self) {
        if self.prio.is_none() {
            self.prio = Some(self.compute_prio());
        }
    }

    fn compute_prio(&self) -> String {
        // Read priority from /proc/[tid]/stat
        let stat_path = format!("/proc/{}/stat", self.tid);
        if let Ok(content) = fs::read_to_string(&stat_path) {
            // Parse the stat file - priority is the 18th field
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() > 17 {
                let nice = parts
                    .get(18)
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);

                // Determine I/O scheduling class and priority
                // For simplicity, we'll show "be/4" (best-effort with priority 4) as default
                // You'd need to use ioprio_get syscall for accurate info
                return format!("be/{}", (20 - nice) / 5);
            }
        }

        "be/4".to_string()
    }

    pub fn get_tgid(&self) -> i32 {
        // Read TGID (parent process ID) from /proc/[tid]/status
        let status = Self::read_proc_status(self.tid);
        if let Some(tgid_str) = status.get("Tgid") {
            if let Some(tgid) = tgid_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<i32>().ok())
            {
                return tgid;
            }
        }
        self.tid // Fallback to tid if we can't find TGID
    }

    pub fn get_cmdline(&self) -> String {
        // Return cached value if available
        if let Some(ref cmdline) = self.cmdline {
            return cmdline.clone();
        }

        // Not cached - compute on the fly (should not happen often)
        self.compute_cmdline()
    }

    pub fn update_cmdline(&mut self) {
        // Only update cmdline if not cached (first time)
        // Note: Processes may exec(), but updating every cycle is expensive
        // We trade accuracy for performance here (like raw-iotop does via caching in UI)
        if self.cmdline.is_none() {
            self.cmdline = Some(self.compute_cmdline());
        }
    }

    // Force refresh cmdline (for when process might have exec'd)
    #[allow(dead_code)]
    pub fn refresh_cmdline(&mut self) {
        self.cmdline = Some(self.compute_cmdline());
    }

    fn compute_cmdline(&self) -> String {
        // Read cmdline from the main process (TGID), not the thread
        let cmdline_path = format!("/proc/{}/cmdline", self.pid);
        let result = if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
            if !cmdline.is_empty() {
                let mut parts: Vec<String> = cmdline
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();

                if !parts.is_empty() {
                    // Strip directory path from first part (show basename only)
                    if parts[0].starts_with('/') {
                        if let Some(pos) = parts[0].rfind('/') {
                            parts[0] = parts[0][pos + 1..].to_string();
                        }
                    }

                    let mut cmd = parts.join(" ").trim().to_string();

                    // For threads: check if thread has a custom name
                    let tgid = self.get_tgid();
                    if tgid != self.tid {
                        // This is a thread, not the main process
                        // Read both status files in one go
                        let tgid_status = Self::read_proc_status(tgid);
                        let thread_status = Self::read_proc_status(self.tid);

                        let tgid_name = tgid_status.get("Name").map(|s| s.to_string());
                        let thread_name = thread_status.get("Name").map(|s| s.to_string());

                        // Add thread name suffix if it's different from the main process name
                        if let (Some(tname), Some(pname)) = (thread_name, tgid_name) {
                            if tname != pname {
                                cmd.push_str(&format!(" [{}]", tname));
                            }
                        }
                    }

                    cmd
                } else {
                    "{no such process}".to_string()
                }
            } else {
                // Kernel thread - get name from status (use tid for kernel threads)
                let status = Self::read_proc_status(self.tid);
                if let Some(name) = status.get("Name") {
                    format!("[{}]", name)
                } else {
                    "{no such process}".to_string()
                }
            }
        } else {
            // Kernel thread - get name from status (use tid for kernel threads)
            let status = Self::read_proc_status(self.tid);
            if let Some(name) = status.get("Name") {
                format!("[{}]", name)
            } else {
                "{no such process}".to_string()
            }
        };

        result
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

    pub fn refresh_processes(&mut self, show_processes: bool) -> Result<((u64, u64), (u64, u64))> {
        let new_timestamp = Instant::now();
        self.duration = new_timestamp.duration_since(self.timestamp).as_secs_f64();
        self.timestamp = new_timestamp;

        let mut total_read = 0u64;
        let mut total_write = 0u64;

        // Read vmstat for actual disk I/O
        let (current_pgpgin, current_pgpgout) = self.read_vmstat().unwrap_or((0, 0));
        let actual_read = if let Some(prev) = self.prev_pgpgin {
            current_pgpgin.saturating_sub(prev)
        } else {
            0
        };
        let actual_write = if let Some(prev) = self.prev_pgpgout {
            current_pgpgout.saturating_sub(prev)
        } else {
            0
        };
        self.prev_pgpgin = Some(current_pgpgin);
        self.prev_pgpgout = Some(current_pgpgout);

        // Mark all threads for cleanup
        for process in self.processes.values_mut() {
            for _thread in process.threads.values_mut() {
                // mark logic would go here if needed
            }
        }

        // Two-stage process listing (like raw-iotop):
        // 1. List all TGIDs (main process IDs) from /proc
        // 2. For each TGID, list threads from /proc/{tgid}/task (only once!)
        let entries = fs::read_dir("/proc")?;

        for entry in entries.flatten() {
            if let Ok(file_name) = entry.file_name().into_string() {
                if let Ok(tgid) = file_name.parse::<i32>() {
                    // Get the ProcessInfo for this TGID
                    let process = self
                        .processes
                        .entry(tgid)
                        .or_insert_with(|| ProcessInfo::new(tgid));

                    // Set tid to tgid for main thread
                    process.tid = tgid;

                    // Get thread IDs for this process (read /proc/{tgid}/task ONCE)
                    let tids = if show_processes {
                        let task_dir = format!("/proc/{}/task", tgid);
                        fs::read_dir(task_dir)
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
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_else(|| vec![tgid])
                    } else {
                        // Thread mode: only show the main thread
                        vec![tgid]
                    };

                    for tid in tids {
                        let thread = process
                            .threads
                            .entry(tid)
                            .or_insert_with(|| ThreadInfo::new(tid));

                        if let Ok(mut conn) = self.taskstats_conn.lock() {
                            if let Ok(Some(stats)) = conn.get_task_stats(tid) {
                                thread.update_stats(stats);
                                let delta = &thread.stats_delta;
                                total_read += delta.read_bytes;
                                total_write += delta.write_bytes;
                            }
                        }
                    }

                    process.update_stats();

                    // Read /proc/{}/status ONCE and extract all needed fields
                    let status = ProcessInfo::read_proc_status(tgid);

                    // Extract and cache UID
                    if let Some(uid_line) = status.get("Uid") {
                        if let Some(uid_str) = uid_line.split_whitespace().next() {
                            if let Ok(uid) = uid_str.parse::<u32>() {
                                if uid != process.uid.unwrap_or(u32::MAX) {
                                    process.user = None;
                                }
                                process.uid = Some(uid);
                            }
                        }
                    }

                    // Extract and cache TGID
                    if let Some(tgid_str) = status.get("Tgid") {
                        if let Some(tgid_val) = tgid_str
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse::<i32>().ok())
                        {
                            process.pid = tgid_val;
                        }
                    }

                    // Cache other metadata to avoid repeated reads during rendering
                    process.update_cmdline();
                    process.update_user();
                    process.update_prio();
                }
            }
        }

        // Remove processes that no longer exist
        self.processes.retain(|_, p| !p.threads.is_empty());

        Ok(((total_read, total_write), (actual_read, actual_write)))
    }
}
