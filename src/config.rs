//! Persistent configuration support.
//!
//! iotop reads its defaults from a TOML configuration file. The file is
//! located (in priority order) at:
//!
//!   * `$IOTOP_CONFIG` (explicit path)
//!   * `$XDG_CONFIG_HOME/iotop/config.toml`
//!   * `~/.config/iotop/config.toml`
//!
//! A missing file is not an error: the built-in defaults are used. A file that
//! exists but fails to parse is an error so the user is not silently misled.
//!
//! Values from the file act as defaults. Any command-line argument that is
//! explicitly provided takes precedence over the file (see [`Config::merge_cli`]).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::ui::SortColumn;

/// Defaults for the interactive sort order, grouped under the `[sort]` table.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct SortDefaults {
    /// Initial sort column. One of: pid, prio, user, read, write, swapin, io, command.
    pub column: SortColumn,
    /// Initial sort direction (true = the natural order of the column).
    pub reverse: bool,
}

impl Default for SortDefaults {
    fn default() -> Self {
        Self {
            column: SortColumn::Pid,
            reverse: true,
        }
    }
}

/// The effective, merged settings that drive the rest of the program.
///
/// [`Config::default`] holds the built-in defaults (identical to the historical
/// command-line defaults). [`Config::load`] overlays the configuration file and
/// [`Config::merge_cli`] lets explicitly-given arguments win over the file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Only show processes/threads actually doing I/O.
    pub only: bool,
    /// Show processes instead of all threads.
    pub processes: bool,
    /// Show accumulated I/O instead of bandwidth.
    pub accumulated: bool,
    /// Delay between iterations in seconds.
    pub delay: f64,
    /// Number of iterations before ending (None = run indefinitely).
    pub iterations: Option<usize>,
    /// Batch (non-interactive) mode.
    pub batch: bool,
    /// Processes/threads to monitor (empty = all).
    pub pid: Vec<i32>,
    /// Users to monitor (username or UID, empty = all).
    pub user: Vec<String>,
    /// Add a timestamp on each output line.
    pub time: bool,
    /// Suppress column names and summary headers.
    pub quiet: bool,
    /// Use kilobytes instead of human-friendly units.
    pub kilobytes: bool,
    /// Emit one JSON object per iteration.
    pub json: bool,
    /// Emit CSV rows.
    pub csv: bool,
    /// Initial interactive sort order.
    pub sort: SortDefaults,
    /// Interactive columns to display (empty = the default set for the kernel).
    pub columns: Vec<SortColumn>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            only: false,
            processes: false,
            accumulated: false,
            delay: 1.0,
            iterations: None,
            batch: false,
            pid: Vec::new(),
            user: Vec::new(),
            time: false,
            quiet: false,
            kilobytes: false,
            json: false,
            csv: false,
            sort: SortDefaults::default(),
            columns: Vec::new(),
        }
    }
}

impl Config {
    /// Load the configuration from the file system, falling back to the built-in
    /// defaults when no configuration file exists.
    pub fn load() -> Result<Self> {
        match config_path() {
            Some(path) => Self::from_file(&path),
            None => Ok(Config::default()),
        }
    }

    /// Read and parse a configuration file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file '{}'", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("failed to parse config file '{}'", path.display()))?;
        Ok(config)
    }

    /// Overlay explicitly-provided command-line arguments on top of the file
    /// defaults. Command-line arguments always take precedence.
    pub fn merge_cli(mut self, args: &crate::Args) -> Self {
        self.only = args.only || self.only;
        self.processes = args.processes || self.processes;
        self.accumulated = args.accumulated || self.accumulated;
        if let Some(delay) = args.delay {
            self.delay = delay;
        }
        if args.iterations.is_some() {
            self.iterations = args.iterations;
        }
        self.batch = args.batch || self.batch;
        if !args.pid.is_empty() {
            self.pid = args.pid.clone();
        }
        if !args.user.is_empty() {
            self.user = args.user.clone();
        }
        self.time = args.time || self.time;
        self.quiet = args.quiet || self.quiet;
        self.kilobytes = args.kilobytes || self.kilobytes;
        self.json = args.json || self.json;
        self.csv = args.csv || self.csv;
        self
    }
}

/// Resolve the configuration file path based on the current environment.
fn config_path() -> Option<PathBuf> {
    config_path_from(
        std::env::var_os("IOTOP_CONFIG").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Pure (testable) version of the path resolution.
///
/// Priority: `IOTOP_CONFIG` wins; otherwise `$XDG_CONFIG_HOME/iotop/config.toml`;
/// otherwise `~/.config/iotop/config.toml`. The last two are only returned when
/// the file actually exists.
fn config_path_from(
    explicit: Option<&OsStr>,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }
    let base = match xdg {
        Some(xdg) => PathBuf::from(xdg),
        None => home.map(|h| PathBuf::from(h).join(".config"))?,
    };
    let path = base.join("iotop").join("config.toml");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args;

    /// An `Args` with every field at its "not explicitly given" value, so a
    /// call to [`Config::merge_cli`] leaves the config defaults untouched.
    fn cli_defaults() -> Args {
        Args {
            only: false,
            processes: false,
            accumulated: false,
            delay: None,
            iterations: None,
            batch: false,
            pid: Vec::new(),
            user: Vec::new(),
            time: false,
            quiet: false,
            kilobytes: false,
            json: false,
            csv: false,
        }
    }

    #[test]
    fn test_default_config_matches_cli_defaults() {
        let cfg = Config::default();
        assert!(!cfg.only);
        assert!(!cfg.processes);
        assert!(!cfg.accumulated);
        assert_eq!(cfg.delay, 1.0);
        assert_eq!(cfg.iterations, None);
        assert!(!cfg.batch);
        assert!(cfg.pid.is_empty());
        assert!(cfg.user.is_empty());
        assert!(!cfg.time);
        assert!(!cfg.quiet);
        assert!(!cfg.kilobytes);
        assert!(!cfg.json);
        assert!(!cfg.csv);
        assert_eq!(cfg.sort.column, SortColumn::Pid);
        assert!(cfg.sort.reverse);
        assert!(cfg.columns.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
            only = true
            processes = true
            accumulated = true
            delay = 2.5
            iterations = 10
            batch = true
            pid = [100, 200]
            user = ["root", "1000"]
            time = true
            quiet = true
            kilobytes = true
            json = true
            csv = true
            columns = ["tid", "read", "write", "command"]

            [sort]
            column = "io"
            reverse = false
        "#;
        let cfg: Config = toml::from_str(toml_str).expect("valid config should parse");
        assert!(cfg.only);
        assert!(cfg.processes);
        assert!(cfg.accumulated);
        assert_eq!(cfg.delay, 2.5);
        assert_eq!(cfg.iterations, Some(10));
        assert!(cfg.batch);
        assert_eq!(cfg.pid, vec![100, 200]);
        assert_eq!(cfg.user, vec!["root".to_string(), "1000".to_string()]);
        assert!(cfg.time);
        assert!(cfg.quiet);
        assert!(cfg.kilobytes);
        assert!(cfg.json);
        assert!(cfg.csv);
        assert_eq!(cfg.sort.column, SortColumn::Io);
        assert!(!cfg.sort.reverse);
        assert_eq!(
            cfg.columns,
            vec![
                SortColumn::Pid,
                SortColumn::Read,
                SortColumn::Write,
                SortColumn::Command
            ]
        );
    }

    #[test]
    fn test_parse_partial_config_uses_defaults() {
        // Only a couple of keys set: everything else must fall back to defaults.
        let toml_str = "delay = 0.5\n[sort]\ncolumn = \"read\"\n";
        let cfg: Config = toml::from_str(toml_str).expect("partial config should parse");
        assert_eq!(cfg.delay, 0.5);
        assert_eq!(cfg.sort.column, SortColumn::Read);
        assert!(cfg.sort.reverse);
        assert!(!cfg.only);
        assert_eq!(cfg.iterations, None);
        assert!(cfg.columns.is_empty());
    }

    #[test]
    fn test_parse_unknown_column_is_error() {
        let toml_str = "[sort]\ncolumn = \"bogus\"\n";
        assert!(toml::from_str::<Config>(toml_str).is_err());
    }

    #[test]
    fn test_parse_invalid_delay_is_error() {
        let toml_str = "delay = \"fast\"\n";
        assert!(toml::from_str::<Config>(toml_str).is_err());
    }

    #[test]
    fn test_merge_cli_overrides_config() {
        let cfg = Config {
            delay: 5.0,
            only: true,
            iterations: Some(99),
            pid: vec![1],
            user: vec!["alice".to_string()],
            ..Config::default()
        };

        let mut args = cli_defaults();
        args.delay = Some(2.0);
        args.iterations = Some(3);
        args.pid = vec![42];
        args.user = vec!["bob".to_string()];
        args.accumulated = true;

        let merged = cfg.clone().merge_cli(&args);
        // CLI explicitly provided values win.
        assert_eq!(merged.delay, 2.0);
        assert_eq!(merged.iterations, Some(3));
        assert_eq!(merged.pid, vec![42]);
        assert_eq!(merged.user, vec!["bob".to_string()]);
        assert!(merged.accumulated);
        // Config-only values are preserved when the CLI does not override them.
        assert!(merged.only);
        assert_eq!(merged.sort.column, cfg.sort.column);
    }

    #[test]
    fn test_merge_cli_keeps_config_defaults() {
        let cfg = Config {
            delay: 5.0,
            only: true,
            ..Config::default()
        };
        let merged = cfg.merge_cli(&cli_defaults());
        assert_eq!(merged.delay, 5.0);
        assert!(merged.only);
        assert!(!merged.processes);
    }

    #[test]
    fn test_sort_column_from_str() {
        assert_eq!("pid".parse::<SortColumn>().unwrap(), SortColumn::Pid);
        assert_eq!("tid".parse::<SortColumn>().unwrap(), SortColumn::Pid);
        assert_eq!("PRIO".parse::<SortColumn>().unwrap(), SortColumn::Prio);
        assert_eq!(" read ".parse::<SortColumn>().unwrap(), SortColumn::Read);
        assert_eq!("write".parse::<SortColumn>().unwrap(), SortColumn::Write);
        assert_eq!("swapin".parse::<SortColumn>().unwrap(), SortColumn::Swapin);
        assert_eq!("io".parse::<SortColumn>().unwrap(), SortColumn::Io);
        assert_eq!(
            "command".parse::<SortColumn>().unwrap(),
            SortColumn::Command
        );
        assert_eq!("cmd".parse::<SortColumn>().unwrap(), SortColumn::Command);
        assert!("nope".parse::<SortColumn>().is_err());
    }

    #[test]
    fn test_config_path_resolution() {
        use std::ffi::OsStr;
        // Explicit path always wins, even if the file is missing.
        assert_eq!(
            config_path_from(Some(OsStr::new("/tmp/custom.toml")), None, None),
            Some(PathBuf::from("/tmp/custom.toml"))
        );
        // No env at all -> no config.
        assert_eq!(config_path_from(None, None, None), None);
        // HOME only -> ~/.config/iotop/config.toml (only when it exists).
        let home = Some(OsStr::new("/nonexistent/home"));
        assert_eq!(config_path_from(None, None, home), None);
    }
}
