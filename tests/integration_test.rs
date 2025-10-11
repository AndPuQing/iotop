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
