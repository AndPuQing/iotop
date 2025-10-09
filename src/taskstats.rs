use anyhow::{Context, Result};
use linux_taskstats::{Client, TaskStats as KernelTaskStats};

// Our TaskStats structure that contains the fields we care about
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskStats {
    pub version: u16,
    pub blkio_delay_total: u64,
    pub swapin_delay_total: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub cancelled_write_bytes: u64,
}

// Global flag to detect if CONFIG_TASK_DELAY_ACCT is enabled
static HAS_DELAY_ACCT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl TaskStats {
    pub fn has_delay_acct() -> bool {
        HAS_DELAY_ACCT.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn from_kernel_stats(stats: &KernelTaskStats) -> Self {
        let blkio_delay = stats.delays.blkio.delay_total.as_nanos() as u64;
        let swapin_delay = stats.delays.swapin.delay_total.as_nanos() as u64;

        // Heuristic to detect if CONFIG_TASK_DELAY_ACCT is enabled
        if !HAS_DELAY_ACCT.load(std::sync::atomic::Ordering::Relaxed) && blkio_delay != 0 {
            HAS_DELAY_ACCT.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        Self {
            version: 0,
            blkio_delay_total: blkio_delay,
            swapin_delay_total: swapin_delay,
            read_bytes: stats.io.read_bytes,
            write_bytes: stats.io.write_bytes,
            cancelled_write_bytes: stats.blkio.cancelled_write_bytes,
        }
    }

    pub fn is_all_zero(&self) -> bool {
        self.blkio_delay_total == 0
            && self.swapin_delay_total == 0
            && self.read_bytes == 0
            && self.write_bytes == 0
            && self.cancelled_write_bytes == 0
    }

    pub fn delta(&self, other: &TaskStats) -> TaskStats {
        TaskStats {
            version: self.version,
            blkio_delay_total: self
                .blkio_delay_total
                .saturating_sub(other.blkio_delay_total),
            swapin_delay_total: self
                .swapin_delay_total
                .saturating_sub(other.swapin_delay_total),
            read_bytes: self.read_bytes.saturating_sub(other.read_bytes),
            write_bytes: self.write_bytes.saturating_sub(other.write_bytes),
            cancelled_write_bytes: self
                .cancelled_write_bytes
                .saturating_sub(other.cancelled_write_bytes),
        }
    }

    pub fn accumulate(&mut self, delta: &TaskStats) {
        self.blkio_delay_total = self
            .blkio_delay_total
            .saturating_add(delta.blkio_delay_total);
        self.swapin_delay_total = self
            .swapin_delay_total
            .saturating_add(delta.swapin_delay_total);
        self.read_bytes = self.read_bytes.saturating_add(delta.read_bytes);
        self.write_bytes = self.write_bytes.saturating_add(delta.write_bytes);
        self.cancelled_write_bytes = self
            .cancelled_write_bytes
            .saturating_add(delta.cancelled_write_bytes);
    }
}

pub struct TaskStatsConnection {
    client: Client,
}

impl TaskStatsConnection {
    pub fn new() -> Result<Self> {
        let client = Client::open().context(
            "Failed to create taskstats client.\n\
             This program requires root privileges or CAP_NET_ADMIN capability.\n\
             Try running with: sudo iotop",
        )?;
        Ok(Self { client })
    }

    pub fn get_task_stats(&mut self, pid: i32) -> Result<Option<TaskStats>> {
        match self.client.pid_stats(pid as u32) {
            Ok(stats) => Ok(Some(TaskStats::from_kernel_stats(&stats))),
            Err(e) => {
                // Process not found or access denied - just return None
                println!("Failed to get task stats for PID {}: {}", pid, e);
                Ok(None)
            }
        }
    }
}
