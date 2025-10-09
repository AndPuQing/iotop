use anyhow::Result;
use nix::unistd::{Uid, User};
use std::collections::HashMap;
use std::fs;
use std::time::Instant;

use crate::taskstats::{TaskStats, TaskStatsConnection};

#[derive(Debug, Clone)]
pub struct ThreadInfo {
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
    pub threads: HashMap<i32, ThreadInfo>,
    pub stats_delta: TaskStats,
    pub stats_accum: TaskStats,
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
            threads: HashMap::new(),
            stats_delta: TaskStats::default(),
            stats_accum: TaskStats::default(),
            stats_accum_timestamp: Instant::now(),
        }
    }

    pub fn get_uid(&mut self) -> Option<u32> {
        if self.uid.is_none() || self.uid == Some(0) {
            // Check current UID
            let status_path = format!("/proc/{}/status", self.pid);
            if let Ok(content) = fs::read_to_string(&status_path) {
                for line in content.lines() {
                    if line.starts_with("Uid:") {
                        if let Some(uid_str) = line.split_whitespace().nth(1) {
                            if let Ok(uid) = uid_str.parse::<u32>() {
                                if uid != self.uid.unwrap_or(u32::MAX) {
                                    self.user = None;
                                    self.uid = Some(uid);
                                }
                                return Some(uid);
                            }
                        }
                    }
                }
            }
        }
        self.uid
    }

    pub fn get_user(&self) -> String {
        if let Some(ref user) = self.user {
            // Truncate to 8 characters like original iotop
            if user.len() > 8 {
                return user.chars().take(8).collect();
            }
            return user.clone();
        }

        // Cache miss - compute it
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
        let status_path = format!("/proc/{}/status", self.tid);
        if let Ok(content) = fs::read_to_string(&status_path) {
            for line in content.lines() {
                if line.starts_with("Tgid:") {
                    if let Some(tgid_str) = line.split_whitespace().nth(1) {
                        if let Ok(tgid) = tgid_str.parse::<i32>() {
                            return tgid;
                        }
                    }
                }
            }
        }
        self.tid // Fallback to tid if we can't find TGID
    }

    pub fn get_cmdline(&self) -> String {
        // Read cmdline from the main process (TGID), not the thread
        let cmdline_path = format!("/proc/{}/cmdline", self.pid);
        if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
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
                        // Get the main process name
                        let tgid_name = if let Ok(status) =
                            fs::read_to_string(format!("/proc/{}/status", tgid))
                        {
                            status
                                .lines()
                                .find(|line| line.starts_with("Name:"))
                                .and_then(|line| line.split(':').nth(1))
                                .map(|name| name.trim().to_string())
                        } else {
                            None
                        };

                        // Get the thread name
                        let thread_name = if let Ok(status) =
                            fs::read_to_string(format!("/proc/{}/status", self.tid))
                        {
                            status
                                .lines()
                                .find(|line| line.starts_with("Name:"))
                                .and_then(|line| line.split(':').nth(1))
                                .map(|name| name.trim().to_string())
                        } else {
                            None
                        };

                        // Add thread name suffix if it's different from the main process name
                        if let (Some(tname), Some(pname)) = (thread_name, tgid_name) {
                            if tname != pname {
                                cmd.push_str(&format!(" [{}]", tname));
                            }
                        }
                    }

                    return cmd;
                }
            }
        }

        // Kernel thread - get name from status (use tid for kernel threads)
        if let Ok(status) = fs::read_to_string(format!("/proc/{}/status", self.tid)) {
            for line in status.lines() {
                if line.starts_with("Name:") {
                    if let Some(name) = line.split(':').nth(1) {
                        return format!("[{}]", name.trim());
                    }
                }
            }
        }

        "{no such process}".to_string()
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

pub struct ProcessList {
    pub processes: HashMap<i32, ProcessInfo>,
    pub taskstats_conn: TaskStatsConnection,
    pub timestamp: Instant,
    pub duration: f64,
    pub prev_pgpgin: Option<u64>,
    pub prev_pgpgout: Option<u64>,
}

impl ProcessList {
    pub fn new(taskstats_conn: TaskStatsConnection) -> Self {
        Self {
            processes: HashMap::new(),
            taskstats_conn,
            timestamp: Instant::now(),
            duration: 0.0,
            prev_pgpgin: None,
            prev_pgpgout: None,
        }
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

    pub fn list_pids(&self, show_processes: bool) -> Result<Vec<i32>> {
        let entries = fs::read_dir("/proc")?;
        let mut pids = Vec::new();

        for entry in entries.flatten() {
            if let Ok(file_name) = entry.file_name().into_string() {
                if let Ok(pid) = file_name.parse::<i32>() {
                    if show_processes {
                        pids.push(pid);
                    } else {
                        // List all threads
                        let task_dir = format!("/proc/{}/task", pid);
                        if let Ok(task_entries) = fs::read_dir(task_dir) {
                            for task_entry in task_entries.flatten() {
                                if let Ok(tid_str) = task_entry.file_name().into_string() {
                                    if let Ok(tid) = tid_str.parse::<i32>() {
                                        pids.push(tid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(pids)
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

        let pids = self.list_pids(show_processes)?;

        for pid in pids {
            let process = self
                .processes
                .entry(pid)
                .or_insert_with(|| ProcessInfo::new(pid));

            // Set tid to pid for threads
            process.tid = pid;

            // Get thread IDs for this process
            let tids = if show_processes {
                let task_dir = format!("/proc/{}/task", pid);
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
                    .unwrap_or_else(|| vec![pid])
            } else {
                vec![pid]
            };

            for tid in tids {
                let thread = process
                    .threads
                    .entry(tid)
                    .or_insert_with(|| ThreadInfo::new(tid));

                if let Ok(Some(stats)) = self.taskstats_conn.get_task_stats(tid) {
                    thread.update_stats(stats);
                    let delta = &thread.stats_delta;
                    total_read += delta.read_bytes;
                    total_write += delta.write_bytes;
                }
            }

            process.update_stats();
            process.get_uid();

            // Update PID to be the TGID (parent process ID)
            process.pid = process.get_tgid();
        }

        // Remove processes that no longer exist
        self.processes.retain(|_, p| !p.threads.is_empty());

        Ok(((total_read, total_write), (actual_read, actual_write)))
    }
}
