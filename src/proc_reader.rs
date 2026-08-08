use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Interval at which runtime-mutable metadata (cmdline, priority) is re-read.
/// Static metadata (UID, user) never expires and is loaded once.
pub const META_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Cache Time-To-Live policy for different data types
#[derive(Debug, Clone, Copy)]
enum CacheTTL {
    /// Never expire - for static data like UID, TGID
    Static,
    /// Expire after duration - for runtime-mutable data like cmdline, priority
    Refresh(Duration),
}

/// A cached entry with timestamp and TTL policy
#[derive(Debug, Clone)]
struct CacheEntry {
    content: String,
    timestamp: Instant,
    ttl: CacheTTL,
}

impl CacheEntry {
    fn new(content: String, ttl: CacheTTL) -> Self {
        Self {
            content,
            timestamp: Instant::now(),
            ttl,
        }
    }

    /// Check if this entry is still valid
    fn is_valid(&self) -> bool {
        match self.ttl {
            CacheTTL::Static => true,
            CacheTTL::Refresh(duration) => self.timestamp.elapsed() < duration,
        }
    }
}

/// Low-level cache for /proc file contents
#[derive(Debug, Clone)]
struct ProcCache {
    cache: HashMap<PathBuf, CacheEntry>,
}

impl ProcCache {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Read a file with caching based on TTL policy
    fn read(&mut self, path: impl Into<PathBuf>, ttl: CacheTTL) -> io::Result<String> {
        let path = path.into();

        // Check cache first
        if let Some(entry) = self.cache.get(&path) {
            if entry.is_valid() {
                return Ok(entry.content.clone());
            }
        }

        // Cache miss - read from disk
        let content = fs::read_to_string(&path)?;
        self.cache
            .insert(path, CacheEntry::new(content.clone(), ttl));

        Ok(content)
    }
}

/// Parsed /proc/[pid]/status data
#[derive(Debug, Clone)]
pub struct ProcStatus {
    pub name: String,
    pub tgid: i32,
    pub pid: i32,
}

impl ProcStatus {
    /// Parse from /proc/[pid]/status content
    fn parse(content: &str) -> Option<Self> {
        let mut name = String::new();
        let mut tgid = 0;
        let mut pid = 0;

        for line in content.lines() {
            if let Some((key, value)) = line.split_once(':') {
                match key.trim() {
                    "Name" => name = value.trim().to_string(),
                    "Tgid" => tgid = value.split_whitespace().next()?.parse().ok()?,
                    "Pid" => pid = value.split_whitespace().next()?.parse().ok()?,
                    _ => {}
                }
            }
        }

        if name.is_empty() || tgid == 0 || pid == 0 {
            return None;
        }

        Some(ProcStatus { name, tgid, pid })
    }
}

/// Bundle of process metadata for initialization
#[derive(Debug, Clone)]
pub struct ProcessMetadata {
    pub pid: i32,
    pub tid: i32,
    pub uid: u32,
    pub cmdline: String,
    pub priority_str: String,
}

/// High-level reader for /proc/[tid] data
///
/// Holds a persistent [`ProcCache`] so cached /proc contents are reused across
/// refresh cycles. `Refresh` TTL entries (e.g. cmdline) are re-read after the
/// interval, while `Static` entries (e.g. status) are loaded once.
#[derive(Debug, Clone)]
pub struct ProcReader {
    tid: i32,
    cache: ProcCache,
}

impl ProcReader {
    pub fn new(tid: i32) -> Self {
        Self {
            tid,
            cache: ProcCache::new(),
        }
    }

    /// Read and parse /proc/[tid]/status
    fn status(&mut self) -> io::Result<ProcStatus> {
        let path = format!("/proc/{}/status", self.tid);
        let content = self.cache.read(path, CacheTTL::Static)?;
        ProcStatus::parse(&content)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Failed to parse status"))
    }

    /// Get a bundle of metadata for process initialization (static + dynamic).
    pub fn metadata_bundle(&mut self, pid: i32) -> Result<ProcessMetadata> {
        // Get UID via fast method
        let uid = self.uid_fast()?;

        let status = self.status()?;
        let (cmdline, priority_str) = self.dynamic_metadata(pid)?;

        Ok(ProcessMetadata {
            pid: status.tgid,
            tid: status.pid,
            uid,
            cmdline,
            priority_str,
        })
    }

    /// Read /proc/[pid]/cmdline with a `Refresh` TTL so it is re-read after
    /// `META_REFRESH_INTERVAL` (e.g. after the process `exec`s a new command).
    fn cmdline(&mut self, pid: i32) -> io::Result<String> {
        let path = format!("/proc/{}/cmdline", pid);
        self.cache
            .read(path, CacheTTL::Refresh(META_REFRESH_INTERVAL))
    }

    /// Read the runtime-mutable metadata (cmdline, priority) through the cache.
    fn dynamic_metadata(&mut self, pid: i32) -> Result<(String, String)> {
        let status = self.status()?;
        let tgid = status.tgid;
        let tid = status.pid;

        // Get priority from ioprio syscall
        let priority_str = super::ioprio::get_ioprio_string(tid);

        // Get cmdline (use TGID for main process cmdline)
        let cmdline_content = self.cmdline(pid)?;
        let cmdline = Self::parse_cmdline(&cmdline_content, pid, tid, &status.name, tgid)?;

        Ok((cmdline, priority_str))
    }

    /// Refresh only the runtime-mutable metadata (cmdline, priority).
    ///
    /// Used for periodic re-reading so that `exec` (new COMMAND) and `renice`
    /// (new PRIO) are reflected. Static fields such as UID are left untouched.
    pub fn refresh_dynamic(&mut self, pid: i32) -> Result<(String, String)> {
        self.dynamic_metadata(pid)
    }

    /// Get UID efficiently via filesystem metadata (no parsing needed)
    fn uid_fast(&self) -> io::Result<u32> {
        let path = format!("/proc/{}", self.tid);
        let metadata = fs::metadata(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(metadata.uid())
        }

        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "UID not available on non-Unix systems",
            ))
        }
    }

    /// Parse cmdline content into a display string
    fn parse_cmdline(
        content: &str,
        pid: i32,
        tid: i32,
        thread_name: &str,
        tgid: i32,
    ) -> Result<String> {
        let cmdline = if !content.is_empty() {
            // Parse null-separated cmdline
            let parts: Vec<&str> = content.split('\0').filter(|s| !s.is_empty()).collect();

            if let Some(&first) = parts.first() {
                // Strip directory path from first part (show basename only)
                // But only if it looks like an actual file path (not something like "sshd-session: user@pts/6")
                let basename = if let Some(slash_pos) = first.rfind('/') {
                    // Check if there's a colon before the slash - if so, this isn't a path
                    let colon_pos = first.find(':');
                    if colon_pos.is_some() && colon_pos.unwrap() < slash_pos {
                        // Colon comes before slash, so this is not a path (e.g., "sshd-session: user@pts/6")
                        first
                    } else {
                        // Normal path, strip directory
                        &first[slash_pos + 1..]
                    }
                } else {
                    first
                };

                let mut cmd = if parts.len() > 1 {
                    format!("{} {}", basename, parts[1..].join(" "))
                } else {
                    basename.to_string()
                };

                // For threads: add thread name suffix if different from main process
                if pid != tid {
                    // Read main process name to compare
                    let tgid_status_path = format!("/proc/{}/status", tgid);
                    if let Ok(tgid_status) = fs::read_to_string(&tgid_status_path) {
                        if let Some(tgid_parsed) = ProcStatus::parse(&tgid_status) {
                            if thread_name != tgid_parsed.name {
                                cmd.push_str(&format!(" [{}]", thread_name));
                            }
                        }
                    }
                }

                cmd
            } else {
                format!("[{}]", thread_name)
            }
        } else {
            // Kernel thread - use name from status
            format!("[{}]", thread_name)
        };

        Ok(cmdline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let mut cache = ProcCache::new();

        // Test that we can read /proc/self/status
        let result = cache.read("/proc/self/status", CacheTTL::Static);
        assert!(result.is_ok());

        // Second read should be a cache hit (same content)
        let result2 = cache.read("/proc/self/status", CacheTTL::Static);
        assert!(result2.is_ok());
        assert_eq!(result.unwrap(), result2.unwrap());
    }

    #[test]
    fn test_cache_static_never_expires() {
        let entry = CacheEntry::new("content".to_string(), CacheTTL::Static);
        std::thread::sleep(Duration::from_millis(5));
        assert!(entry.is_valid());
    }

    #[test]
    fn test_cache_refresh_ttl_expires() {
        // A Refresh entry with a 1ms TTL should expire after the interval.
        let entry = CacheEntry::new(
            "content".to_string(),
            CacheTTL::Refresh(Duration::from_millis(1)),
        );
        assert!(entry.is_valid());
        std::thread::sleep(Duration::from_millis(5));
        assert!(!entry.is_valid());
    }

    #[test]
    fn test_parse_status() {
        let content = "Name:\ttest\nTgid:\t1234\nPid:\t1234\nPPid:\t1\n";
        let status = ProcStatus::parse(content);
        assert!(status.is_some());
        let status = status.unwrap();
        assert_eq!(status.name, "test");
        assert_eq!(status.tgid, 1234);
        assert_eq!(status.pid, 1234);
    }

    #[test]
    fn test_parse_cmdline_normal_path() {
        // Test normal executable path - should strip directory
        let cmdline = "/usr/bin/bash\0-l\0";
        let result = ProcReader::parse_cmdline(cmdline, 1234, 1234, "bash", 1234);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "bash -l");
    }

    #[test]
    fn test_parse_cmdline_with_colon() {
        // Test sshd-session style - should NOT strip after colon
        let cmdline = "sshd-session: happy@pts/6\0";
        let result = ProcReader::parse_cmdline(cmdline, 1234, 1234, "sshd-session", 1234);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sshd-session: happy@pts/6");
    }

    #[test]
    fn test_parse_cmdline_sshd_listener() {
        // Test sshd listener style - should NOT strip after colon
        let cmdline = "sshd: /usr/bin/sshd\0-D\0";
        let result = ProcReader::parse_cmdline(cmdline, 1234, 1234, "sshd", 1234);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sshd: /usr/bin/sshd -D");
    }

    #[test]
    fn test_parse_cmdline_no_path() {
        // Test command with no path separator
        let cmdline = "python\0script.py\0";
        let result = ProcReader::parse_cmdline(cmdline, 1234, 1234, "python", 1234);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "python script.py");
    }

    #[test]
    fn test_metadata_bundle_self() {
        // metadata_bundle must work against /proc/self without root privileges.
        let pid = std::process::id() as i32;
        let mut reader = ProcReader::new(pid);
        let meta = reader
            .metadata_bundle(pid)
            .expect("metadata_bundle should succeed for the current process");

        // For the running test binary tgid == pid == tid.
        assert_eq!(meta.pid, pid);
        assert_eq!(meta.tid, pid);

        // UID should match the filesystem metadata of our own /proc entry.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let fs_uid = std::fs::metadata(format!("/proc/{}", pid))
                .expect("own /proc entry must exist")
                .uid();
            assert_eq!(meta.uid, fs_uid);
        }

        // cmdline must parse to a non-empty display string.
        assert!(!meta.cmdline.is_empty());
        // priority string must be resolvable for our own process.
        assert!(!meta.priority_str.is_empty());
    }

    #[test]
    fn test_metadata_bundle_matches_own_status() {
        // Cross-check the parsed pid/tgid against /proc/self/status.
        let pid = std::process::id() as i32;
        let mut reader = ProcReader::new(pid);
        let meta = reader
            .metadata_bundle(pid)
            .expect("metadata_bundle should succeed");

        let status = ProcReader::new(pid)
            .status()
            .expect("status should parse for the current process");
        assert_eq!(meta.tid, status.pid);
        assert_eq!(meta.pid, status.tgid);
    }

    #[test]
    fn test_parse_status_rejects_malformed() {
        // Missing required fields must yield None.
        assert!(ProcStatus::parse("Name:\tfoo\n").is_none());
        assert!(ProcStatus::parse("Tgid:\t1\nPid:\t1\n").is_none());
        assert!(ProcStatus::parse("").is_none());
    }
}
