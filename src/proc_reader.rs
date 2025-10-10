use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Cache Time-To-Live policy for different data types
#[derive(Debug, Clone, Copy)]
enum CacheTTL {
    /// Never expire - for static data like UID, TGID, cmdline
    Static,
    /// Expire after duration - for semi-dynamic data like priority
    Refresh(Duration),
}

/// A cached entry with timestamp and TTL policy
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
                    "Tgid" => tgid = value.trim().split_whitespace().next()?.parse().ok()?,
                    "Pid" => pid = value.trim().split_whitespace().next()?.parse().ok()?,
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

    /// Extract nice value from /proc/[tid]/stat
    fn read_nice(&mut self) -> io::Result<i32> {
        let path = format!("/proc/{}/stat", self.tid);
        let content = self
            .cache
            .read(path, CacheTTL::Refresh(Duration::from_secs(2)))?;

        // Parse stat file to extract nice value (field 19, 0-indexed field 18)
        // Format: pid (comm) state ... priority nice ...
        let _start = content
            .find('(')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid stat format"))?;
        let end = content
            .rfind(')')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid stat format"))?;

        let rest = &content[end + 1..];
        let parts: Vec<&str> = rest.split_whitespace().collect();

        if parts.len() < 17 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Stat too short"));
        }

        parts[16]
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Failed to parse nice value"))
    }

    /// Read /proc/[tid]/cmdline
    fn cmdline(&mut self, pid: i32) -> io::Result<String> {
        let path = format!("/proc/{}/cmdline", pid);
        self.cache.read(path, CacheTTL::Static)
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

    /// Get a bundle of metadata for process initialization
    pub fn metadata_bundle(&mut self, pid: i32) -> Result<ProcessMetadata> {
        // Get UID via fast method
        let uid = self.uid_fast()?;

        // Get TGID and other info from status
        let status = self.status()?;
        let tgid = status.tgid;
        let tid = status.pid;

        // Get priority from stat (only extract nice value)
        let nice = self.read_nice()?;
        let priority_str = format!("be/{}", (20 - nice) / 5);

        // Get cmdline (use TGID for main process cmdline)
        let cmdline_content = self.cmdline(pid)?;
        let cmdline = Self::parse_cmdline(&cmdline_content, pid, tid, &status.name, tgid)?;

        Ok(ProcessMetadata {
            pid: tgid,
            tid,
            uid,
            cmdline,
            priority_str,
        })
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
}
