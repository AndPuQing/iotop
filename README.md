# iotop - Rust Implementation

A modern, high-performance Rust implementation of iotop - a tool to monitor I/O usage of processes on Linux.

![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/AndPuQing/iotop/ci.yaml?style=flat-square&logo=github)
![Crates.io Version](https://img.shields.io/crates/v/iotop?style=flat-square&logo=rust)
![Crates.io Downloads (recent)](https://img.shields.io/crates/dr/iotop?style=flat-square)
![dependency status](https://deps.rs/repo/github/AndPuQing/iotop/status.svg?style=flat-square)
![Crates.io License](https://img.shields.io/crates/l/iotop?style=flat-square) ![Crates.io Size](https://img.shields.io/crates/size/iotop?style=flat-square)

## Snapshot

<div align="center">
 <img src="https://github.com/AndPuQing/iotop/blob/main/assets/Snapshot.png?raw=true">
</div>

## Installation

### From Cargo (Recommended)

Install directly from [crates.io](https://crates.io/crates/iotop):
```bash
cargo install iotop
```

### From Source

Clone and build from source:
```bash
git clone https://github.com/AndPuQing/iotop.git
cd iotop
cargo build --release
```

The binary will be available at `./target/release/iotop`.

### System-wide Installation

#### Option 1: Copy binary to system path
```bash
cargo build --release
sudo cp target/release/iotop /usr/local/bin/
sudo cp doc/iotop.8 /usr/share/man/man8/
sudo mandb  # Update man page database
```

#### Option 2: Using cargo install with prefix
```bash
cargo install --path . --root /usr/local
sudo cp doc/iotop.8 /usr/share/man/man8/
sudo mandb
```

### Permissions Setup

iotop requires root privileges to access the kernel's taskstats interface. You have two options:

#### Option 1: Run with sudo (simplest)
```bash
sudo iotop
```

#### Option 2: Grant CAP_NET_ADMIN capability (no sudo needed)
```bash
# Allow iotop to run without sudo by granting the required capability
sudo setcap cap_net_admin+eip /usr/local/bin/iotop

# Now you can run without sudo
iotop
```

### Enable Kernel Delay Accounting

For full functionality (SWAPIN and IO columns), enable kernel delay accounting:
```bash
# Temporary (until reboot)
sudo sysctl -w kernel.task_delayacct=1

# Permanent (survives reboot)
echo "kernel.task_delayacct = 1" | sudo tee -a /etc/sysctl.conf
sudo sysctl -p
```

## Usage

### Basic Examples

Run interactively:
```bash
sudo iotop
```

Show only processes doing I/O:
```bash
sudo iotop -o
```

Run in batch mode (5 iterations, 2 second delay):
```bash
sudo iotop -b -n 5 -d 2
```

Monitor specific process:
```bash
sudo iotop -p 1234
```

Monitor user's processes:
```bash
sudo iotop -u www-data
```

Batch mode with timestamps:
```bash
sudo iotop -t -b -n 10 > iotop.log
```

### Command-Line Options

| Option | Long Form | Description |
|--------|-----------|-------------|
| `-o` | `--only` | Only show processes or threads actually doing I/O |
| `-P` | `--processes` | Show processes instead of all threads |
| `-a` | `--accumulated` | Show accumulated I/O instead of bandwidth |
| `-d` | `--delay` | Delay between iterations in seconds [default: 1.0] |
| `-n` | `--iterations` | Number of iterations before ending (infinite if not specified) |
| `-b` | `--batch` | Batch mode (non-interactive) |
| `-p` | `--pid` | Monitor specific processes/threads (can be repeated) |
| `-u` | `--user` | Monitor processes by username or UID (can be repeated) |
| `-t` | `--time` | Add timestamp on each line (implies `--batch`) |
| `-q` | `--quiet` | Suppress column names and headers (implies `--batch`) |
| `-k` | `--kilobytes` | Use kilobytes instead of human-friendly units |

### Interactive Mode Controls

When running in interactive mode (default), you can use the following keyboard shortcuts:

| Key | Action |
|-----|--------|
| `q` / `Q` / `Ctrl+C` | Quit the program |
| `o` / `O` | Toggle showing only processes doing I/O |
| `a` / `A` | Toggle between bandwidth and accumulated I/O |
| `p` / `P` | Toggle between showing processes and threads |
| `r` / `R` | Reverse the current sort order |
| `Space` | Pause/resume display updates |
| `Left` / `Right` | Cycle through sort columns |
| `Up` / `Down` | Scroll through process list |
| `PageUp` / `PageDown` | Scroll by 10 rows |
| `Home` | Jump to first sort column (or first row with Ctrl) |
| `End` | Jump to last sort column (or last row with Ctrl) |

Mouse wheel scrolling is also supported for navigating the process list.

## Architecture

This implementation uses:
- **Netlink Taskstats**: Interfaces with the Linux kernel's taskstats interface via netlink sockets
- **Procfs**: Reads process information from `/proc` filesystem
- **Async/Await**: Tokio-based async runtime for concurrent data collection
- **TUI Framework**: Crossterm + Ratatui for terminal rendering

### Development

Run the project in development mode:

```bash
# Build and run
cargo run -- [options]

# Build in release mode
cargo build --release

# Run tests
cargo test

# Check for issues
cargo clippy

# Format code
cargo fmt

# Run with custom options
cargo run -- -o -d 2
```

### License

MIT License - See LICENSE file for details.

### References

- Original Python implementation: http://guichaz.free.fr/iotop/
- C version by Tomas M: https://github.com/Tomas-M/iotop
- Linux Taskstats documentation: https://www.kernel.org/doc/Documentation/accounting/taskstats.txt

### Contributing

Contributions are welcome! This is an ongoing migration project. Areas for contribution include:

- Additional test coverage
- Performance optimizations
- Documentation improvements
- Bug fixes and feature requests

Please open an issue or pull request on [GitHub](https://github.com/AndPuQing/iotop).
