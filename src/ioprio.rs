use anyhow::Result;
use std::fmt;

// Constants from Linux kernel
const IOPRIO_CLASS_SHIFT: u32 = 13;
const IOPRIO_PRIO_MASK: u32 = (1 << IOPRIO_CLASS_SHIFT) - 1;

const IOPRIO_WHO_PROCESS: i32 = 1;

// I/O priority classes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoprioClass {
    None = 0,
    RealTime = 1,
    BestEffort = 2,
    Idle = 3,
}

impl IoprioClass {
    fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(IoprioClass::None),
            1 => Some(IoprioClass::RealTime),
            2 => Some(IoprioClass::BestEffort),
            3 => Some(IoprioClass::Idle),
            _ => None,
        }
    }

    fn as_str(&self) -> &str {
        match self {
            IoprioClass::None => "none",
            IoprioClass::RealTime => "rt",
            IoprioClass::BestEffort => "be",
            IoprioClass::Idle => "idle",
        }
    }
}

// I/O priority value
#[derive(Debug, Clone, Copy)]
pub struct Ioprio {
    pub class: IoprioClass,
    pub data: u32,
}

impl Ioprio {
    pub fn new(class: IoprioClass, data: u32) -> Self {
        Self { class, data }
    }

    pub fn from_raw(ioprio: i32) -> Self {
        let class_val = ((ioprio as u32) >> IOPRIO_CLASS_SHIFT) & 0x7;
        let data = (ioprio as u32) & IOPRIO_PRIO_MASK;

        let class = IoprioClass::from_u32(class_val).unwrap_or(IoprioClass::None);

        Self { class, data }
    }

    pub fn to_raw(self) -> i32 {
        (((self.class as u32) << IOPRIO_CLASS_SHIFT) | self.data) as i32
    }

    #[allow(dead_code)]
    pub fn from_string(s: &str) -> Result<Self> {
        if s == "idle" {
            return Ok(Self::new(IoprioClass::Idle, 0));
        }

        if let Some((class_str, data_str)) = s.split_once('/') {
            let class = match class_str {
                "rt" => IoprioClass::RealTime,
                "be" => IoprioClass::BestEffort,
                _ => anyhow::bail!("Invalid I/O priority class: {}", class_str),
            };

            let data: u32 = data_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid I/O priority data: {}", data_str))?;

            if data > 7 {
                anyhow::bail!("I/O priority data must be 0-7, got {}", data);
            }

            Ok(Self::new(class, data))
        } else {
            anyhow::bail!("Invalid I/O priority format: {}", s)
        }
    }
}

impl fmt::Display for Ioprio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.class {
            IoprioClass::None => write!(f, "none"),
            IoprioClass::Idle => write!(f, "idle"),
            IoprioClass::RealTime | IoprioClass::BestEffort => {
                write!(f, "{}/{}", self.class.as_str(), self.data)
            }
        }
    }
}

// Get I/O priority for a process
pub fn get_ioprio(pid: i32) -> Result<Ioprio> {
    // Try using syscall
    let result = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, pid) };

    if result < 0 {
        // If syscall fails, try fallback method
        return get_ioprio_from_sched(pid);
    }

    let ioprio = Ioprio::from_raw(result as i32);

    // If class is None, it means no explicit I/O priority is set
    // Fall back to deriving from scheduler/nice value (like original iotop)
    if matches!(ioprio.class, IoprioClass::None) {
        return get_ioprio_from_sched(pid);
    }

    Ok(ioprio)
}

// Fallback: get I/O priority from scheduler info
fn get_ioprio_from_sched(pid: i32) -> Result<Ioprio> {
    // Get scheduler policy
    let policy = unsafe { libc::sched_getscheduler(pid) };

    if policy < 0 {
        anyhow::bail!("Failed to get scheduler for PID {}", pid);
    }

    // Get nice value
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid as u32) };

    // Convert nice to ioprio data (0-7 scale)
    let ioprio_data = ((nice + 20) / 5).clamp(0, 7) as u32;

    // Determine class based on scheduler
    let class = match policy {
        libc::SCHED_FIFO | libc::SCHED_RR => IoprioClass::RealTime,
        libc::SCHED_IDLE => IoprioClass::Idle,
        _ => IoprioClass::BestEffort,
    };

    Ok(Ioprio::new(class, ioprio_data))
}

// Set I/O priority for a process
#[allow(dead_code)]
pub fn set_ioprio(pid: i32, ioprio: Ioprio) -> Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            pid,
            ioprio.to_raw(),
        )
    };

    if result < 0 {
        let errno = unsafe { *libc::__errno_location() };
        anyhow::bail!(
            "Failed to set I/O priority for PID {}: {}",
            pid,
            std::io::Error::from_raw_os_error(errno)
        );
    }

    Ok(())
}

// Get priority string for display (with fallback for errors)
pub fn get_ioprio_string(pid: i32) -> String {
    match get_ioprio(pid) {
        Ok(ioprio) => ioprio.to_string(),
        Err(_) => "?err".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ioprio_from_string() {
        assert!(Ioprio::from_string("be/4").is_ok());
        assert!(Ioprio::from_string("rt/0").is_ok());
        assert!(Ioprio::from_string("idle").is_ok());
        assert!(Ioprio::from_string("invalid").is_err());
    }

    #[test]
    fn test_ioprio_display() {
        let ioprio = Ioprio::new(IoprioClass::BestEffort, 4);
        assert_eq!(ioprio.to_string(), "be/4");

        let ioprio = Ioprio::new(IoprioClass::Idle, 0);
        assert_eq!(ioprio.to_string(), "idle");
    }

    #[test]
    fn test_ioprio_raw_conversion() {
        let ioprio = Ioprio::new(IoprioClass::BestEffort, 4);
        let raw = ioprio.to_raw();
        let parsed = Ioprio::from_raw(raw);

        assert_eq!(parsed.class, ioprio.class);
        assert_eq!(parsed.data, ioprio.data);
    }
}
