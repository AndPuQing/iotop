# iotop - Rust Implementation

A Rust implementation of iotop, a tool to monitor I/O usage of processes on Linux.

### Usage

Build the project:
```bash
cargo build --release
```

Run interactively (requires root for netlink access):
```bash
sudo cargo run --release
```

Run in batch mode:
```bash
sudo cargo run --release -- -b -n 5
```

Show only active processes:
```bash
sudo cargo run --release -- -o
```

Show help:
```bash
cargo run -- --help
```

### Options

```
-o, --only               Only show processes or threads actually doing I/O
-P, --processes          Show processes, not all threads
-a, --accumulated        Show accumulated I/O instead of bandwidth
-d, --delay <DELAY>      Delay between iterations in seconds [default: 1.0]
-n, --iter <ITERATIONS>  Number of iterations before ending (infinite if not specified)
-b, --batch              Batch mode (non-interactive)
```


### Development

The project uses standard Rust tooling:

```bash
# Build
cargo build

# Run tests (when added)
cargo test

# Check for issues
cargo clippy

# Format code
cargo fmt
```

### License

MIT License

### Original Python Version

The original Python implementation can be found at:
- Upstream: https://github.com/Tomas-M/iotop (C version)
- Original Python: http://guichaz.free.fr/iotop/

### Contributing

This is a migration project. Contributions to missing features are welcome.
