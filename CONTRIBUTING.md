# Contributing to iotop

Thank you for your interest in contributing to iotop! This document provides guidelines and instructions for contributing.

## Code of Conduct

By participating in this project, you agree to abide by our Code of Conduct (see CODE_OF_CONDUCT.md).

## How to Contribute

### Reporting Bugs

Before creating bug reports, please check existing issues to avoid duplicates. When creating a bug report, include:

- **Description**: A clear and concise description of the bug
- **Steps to Reproduce**: Detailed steps to reproduce the behavior
- **Expected Behavior**: What you expected to happen
- **Actual Behavior**: What actually happened
- **Environment**:
  - OS and version (e.g., Ubuntu 22.04, Arch Linux)
  - Kernel version (`uname -r`)
  - iotop version
  - Rust version (`rustc --version`)
- **Additional Context**: Any other relevant information (logs, screenshots, etc.)

### Suggesting Enhancements

Enhancement suggestions are welcome! Please include:

- **Clear Description**: What enhancement you'd like to see
- **Use Case**: Why this would be useful
- **Alternatives**: Other solutions you've considered
- **Examples**: If applicable, examples from similar tools

### Pull Requests

1. **Fork the Repository**
   ```bash
   git clone https://github.com/AndPuQing/iotop.git
   cd iotop
   ```

2. **Create a Branch**
   ```bash
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/your-bug-fix
   ```

3. **Make Your Changes**
   - Write clear, documented code
   - Follow the existing code style
   - Add tests for new functionality
   - Update documentation as needed

4. **Test Your Changes**
   ```bash
   # Run tests
   cargo test

   # Check formatting
   cargo fmt --all --check

   # Run clippy
   cargo clippy --all-targets --all-features -- -D warnings

   # Test the binary
   cargo build --release
   sudo ./target/release/iotop -b -n 1
   ```

5. **Commit Your Changes**
   - Use clear, descriptive commit messages
   - Follow conventional commit format:
     - `feat: add new feature`
     - `fix: resolve bug`
     - `docs: update documentation`
     - `test: add tests`
     - `refactor: code refactoring`
     - `perf: performance improvements`

6. **Push and Create Pull Request**
   ```bash
   git push origin feature/your-feature-name
   ```
   Then create a pull request on GitHub.

## Development Setup

### Prerequisites

- Rust 1.70 or later
- Linux kernel 2.6.20 or later
- Root access or CAP_NET_ADMIN capability for testing

### Building

```bash
cargo build
```

### Running Tests

```bash
# Unit tests
cargo test --lib

# Integration tests (requires root)
sudo cargo test --test integration_test

# All tests
cargo test
```

### Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Follow Rust naming conventions
- Add documentation comments for public APIs
- Keep functions focused and reasonably sized

### Testing Guidelines

- Add unit tests for new functions
- Add integration tests for new features
- Ensure tests pass before submitting PR
- Aim for good test coverage

## Project Structure

```
iotop/
├── src/
│   ├── main.rs         # Entry point and CLI argument parsing
│   ├── process.rs      # Process management and I/O tracking
│   ├── proc_reader.rs  # /proc filesystem reading
│   ├── taskstats.rs    # Linux taskstats netlink interface
│   ├── ioprio.rs       # I/O priority handling
│   ├── ui.rs           # Terminal UI (ratatui)
│   └── lib.rs          # Library exports
├── tests/              # Integration tests
├── doc/                # Man pages
├── completions/        # Shell completions
└── patches/            # Patched dependencies
```

## Areas for Contribution

We welcome contributions in these areas:

- **Performance Optimization**: Improving efficiency and reducing overhead
- **Feature Additions**: New filtering options, output formats, etc.
- **Bug Fixes**: Fixing reported issues
- **Documentation**: Improving docs, examples, and comments
- **Testing**: Adding test coverage
- **Platform Support**: Testing on different Linux distributions
- **Packaging**: Creating packages for various distributions

## Questions?

Feel free to:
- Open an issue for questions
- Start a discussion on GitHub Discussions
- Check existing issues and PRs

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

## Recognition

Contributors will be recognized in:
- Git commit history
- Release notes
- Project documentation

Thank you for contributing to iotop!
