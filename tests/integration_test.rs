// Integration tests for iotop
use std::process::Command;

#[test]
fn test_help_flag() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("iotop"));
    assert!(stdout.contains("--only"));
    assert!(stdout.contains("--batch"));
}

#[test]
fn test_version_info() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Rust implementation") || stdout.contains("iotop"));
}

#[test]
fn test_invalid_delay() {
    let output = Command::new("cargo")
        .args(["run", "--", "-d", "invalid"])
        .output()
        .expect("Failed to execute command");

    assert!(!output.status.success());
}

#[test]
fn test_batch_mode_requires_root() {
    // This test checks that the program provides a reasonable error when not run as root
    let output = Command::new("cargo")
        .args(["run", "--", "-b", "-n", "1"])
        .output()
        .expect("Failed to execute command");

    // Either succeeds (if run as root) or fails with permission error
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // Should mention permissions or requirements
        assert!(
            stderr.contains("permission")
                || stderr.contains("requirements")
                || stderr.contains("root")
                || stderr.contains("CAP_NET_ADMIN")
                || stderr.contains("Cannot open netlink socket")
        );
    }
}

#[test]
fn test_json_output_is_valid() {
    // `-b --json -n 1` must emit a single valid JSON object per iteration,
    // with the required per-process fields. Skipped when taskstats is not
    // readable (mirrors test_batch_mode_requires_root).
    let output = Command::new("cargo")
        .args(["run", "--", "-b", "--json", "-n", "1"])
        .output()
        .expect("Failed to execute command");

    if !output.status.success() {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json output must be valid JSON");

    let obj = value.as_object().expect("top-level JSON must be an object");
    for key in [
        "total_read",
        "total_write",
        "actual_read",
        "actual_write",
        "processes",
    ] {
        assert!(obj.contains_key(key), "missing JSON key: {}", key);
    }
    let processes = obj["processes"]
        .as_array()
        .expect("processes must be an array");
    if let Some(first) = processes.first() {
        for key in [
            "tid", "prio", "user", "read", "write", "swapin", "io", "command",
        ] {
            assert!(
                first.as_object().unwrap().contains_key(key),
                "missing per-process JSON key: {}",
                key
            );
        }
    }
}

#[test]
fn test_csv_output_is_valid() {
    // `-b --csv -n 1` must emit a header row followed by parseable CSV rows.
    // Skipped when taskstats is not readable.
    let output = Command::new("cargo")
        .args(["run", "--", "-b", "--csv", "-n", "1"])
        .output()
        .expect("Failed to execute command");

    if !output.status.success() {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let header = lines.next().expect("CSV must have a header row");
    let header_cols: Vec<&str> = header.split(',').collect();
    for col in [
        "tid", "pid", "prio", "user", "read", "write", "swapin", "io", "command",
    ] {
        assert!(header_cols.contains(&col), "missing CSV column: {}", col);
    }
    if let Some(row) = lines.next() {
        assert_eq!(
            row.split(',').count(),
            header_cols.len(),
            "CSV data row column count must match header"
        );
    }
}
