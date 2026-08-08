use anyhow::Result;
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

impl TaskStats {
    // Detect whether CONFIG_TASK_DELAY_ACCT is enabled by reading the kernel
    // sysctl directly (value 1 means enabled), instead of relying on the
    // post-hoc heuristic of observing a non-zero blkio_delay.
    pub fn has_delay_acct() -> bool {
        std::fs::read_to_string("/proc/sys/kernel/task_delayacct")
            .map(|contents| contents.trim() == "1")
            .unwrap_or(false)
    }

    pub fn from_kernel_stats(stats: &KernelTaskStats) -> Self {
        let blkio_delay = stats.delays.blkio.delay_total.as_nanos() as u64;
        let swapin_delay = stats.delays.swapin.delay_total.as_nanos() as u64;

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

/// Result of probing whether taskstats is usable with the current privileges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskstatsAccess {
    /// taskstats is accessible and I/O statistics can be read.
    Accessible,
    /// taskstats exists but the current user lacks permission to read it.
    PermissionDenied,
    /// taskstats is not supported by the running kernel.
    Unsupported,
}

pub struct TaskStatsConnection {
    client: Client,
}

impl TaskStatsConnection {
    pub fn new() -> Result<Self> {
        let client = match Client::open() {
            Ok(client) => client,
            Err(linux_taskstats::Error::NoFamilyId) => {
                anyhow::bail!(
                    "Could not run iotop: the kernel does not support taskstats\n\
                     (missing CONFIG_TASKSTATS). I/O statistics cannot be collected."
                );
            }
            Err(err) => {
                anyhow::bail!(
                    "Failed to create the taskstats client: {err}\n\
                     This program requires root privileges or the CAP_NET_ADMIN capability.\n\
                     Try running with: sudo iotop"
                );
            }
        };
        Ok(Self { client })
    }

    /// Probe whether taskstats is readable with the current privileges.
    ///
    /// `Client::open()` already succeeded, so the kernel exposes taskstats;
    /// a failure to read our own statistics therefore almost always means the
    /// caller lacks the required permission (root or CAP_NET_ADMIN), rather
    /// than that the kernel is unsupported.
    pub fn probe_access(&self) -> TaskstatsAccess {
        match self.client.pid_stats(std::process::id()) {
            Ok(_) => TaskstatsAccess::Accessible,
            Err(linux_taskstats::Error::Netlink(_)) => TaskstatsAccess::PermissionDenied,
            Err(_) => TaskstatsAccess::Unsupported,
        }
    }

    pub fn get_task_stats(&mut self, pid: i32) -> Result<Option<TaskStats>> {
        match self.client.pid_stats(pid as u32) {
            Ok(stats) => Ok(Some(TaskStats::from_kernel_stats(&stats))),
            Err(_) => {
                // Process not found or access denied - just return None
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(
        read_bytes: u64,
        write_bytes: u64,
        blkio: u64,
        swapin: u64,
        cancelled: u64,
    ) -> TaskStats {
        TaskStats {
            version: 0,
            blkio_delay_total: blkio,
            swapin_delay_total: swapin,
            read_bytes,
            write_bytes,
            cancelled_write_bytes: cancelled,
        }
    }

    #[test]
    fn test_delta_subtracts_correctly() {
        let prev = stats(100, 200, 1000, 500, 50);
        let curr = stats(300, 400, 1500, 700, 80);

        let d = curr.delta(&prev);
        assert_eq!(d.read_bytes, 200);
        assert_eq!(d.write_bytes, 200);
        assert_eq!(d.blkio_delay_total, 500);
        assert_eq!(d.swapin_delay_total, 200);
        assert_eq!(d.cancelled_write_bytes, 30);
    }

    #[test]
    fn test_delta_saturates_at_zero() {
        // A counter going backwards (e.g. a kernel reset) must never underflow.
        let prev = stats(1000, 1000, 1000, 1000, 1000);
        let curr = stats(100, 100, 100, 100, 100);

        let d = curr.delta(&prev);
        assert_eq!(d.read_bytes, 0);
        assert_eq!(d.write_bytes, 0);
        assert_eq!(d.blkio_delay_total, 0);
        assert_eq!(d.swapin_delay_total, 0);
        assert_eq!(d.cancelled_write_bytes, 0);
    }

    #[test]
    fn test_delta_of_equal_stats_is_zero() {
        let s = stats(10, 20, 30, 40, 50);
        assert!(s.delta(&s).is_all_zero());
    }

    #[test]
    fn test_accumulate_adds_fields() {
        let mut acc = TaskStats::default();
        acc.accumulate(&stats(1, 2, 3, 4, 5));
        acc.accumulate(&stats(10, 20, 30, 40, 50));

        assert_eq!(acc.read_bytes, 11);
        assert_eq!(acc.write_bytes, 22);
        assert_eq!(acc.blkio_delay_total, 33);
        assert_eq!(acc.swapin_delay_total, 44);
        assert_eq!(acc.cancelled_write_bytes, 55);
    }

    #[test]
    fn test_accumulate_saturates_at_max() {
        let mut acc = stats(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        acc.accumulate(&stats(1, 1, 1, 1, 1));

        assert_eq!(acc.read_bytes, u64::MAX);
        assert_eq!(acc.write_bytes, u64::MAX);
        assert_eq!(acc.blkio_delay_total, u64::MAX);
        assert_eq!(acc.swapin_delay_total, u64::MAX);
        assert_eq!(acc.cancelled_write_bytes, u64::MAX);
    }

    #[test]
    fn test_is_all_zero() {
        assert!(TaskStats::default().is_all_zero());
        assert!(!stats(1, 0, 0, 0, 0).is_all_zero());
        assert!(!stats(0, 1, 0, 0, 0).is_all_zero());
        assert!(!stats(0, 0, 1, 0, 0).is_all_zero());
        assert!(!stats(0, 0, 0, 1, 0).is_all_zero());
        assert!(!stats(0, 0, 0, 0, 1).is_all_zero());
    }
}
