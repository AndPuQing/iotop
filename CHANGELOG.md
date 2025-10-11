# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.8] - 2025-10-11

### Added
- Renovate configuration for automated dependency management
- Mouse scroll support in the UI
- Current time display in the title header

### Changed
- Enhanced process table rendering
- Improved header rendering with integrated timestamp

### Fixed
- Fixed formatting in TaskStats buffer initialization

## [0.1.7] - 2025-10-10

### Changed
- Set default-features to false for argh, nix, crossterm, and linux-taskstats dependencies
- Optimized binary size and build configuration

## [0.1.6] - 2025-10-10

### Added
- I/O priority handling with Ioprio module
- Process priority retrieval through ProcReader integration

### Changed
- Enhanced header rendering with adjusted height

## [0.1.5] - 2025-10-09

### Added
- PID and UID filtering options
- Timestamp display integration
- Current time display in UI header

### Changed
- Enhanced process monitoring features

## [0.1.4] - 2025-10-09

### Added
- ProcReader module for efficient process metadata handling
- justfile for build and run commands

### Changed
- Refactored process table rendering and sorting
- Enhanced header definitions and styles
- Improved process info retrieval
- Simplified ProcStatus parsing logic for Tgid and Pid extraction

### Removed
- Unused import of Title widget from ui.rs

## [0.1.3] - 2025-10-08

### Changed
- Updated bindgen version to 0.65.1
- Refactored dependencies in Cargo.toml for consistency
- Improved code readability and maintainability

### Removed
- Unused functions in ProcessInfo

## [0.1.2] - 2025-10-08

### Added
- Initial public release

### Changed
- Enhanced UI sorting functionality
- Improved process information retrieval
- Refactored process management and UI for better performance

## [0.1.0] - 2025-10-08

### Added
- Initial Rust implementation of iotop
- Interactive TUI for monitoring I/O usage
- Batch mode support
- Process and thread monitoring
- I/O bandwidth and accumulated I/O tracking
- Command-line options for filtering and customization

[Unreleased]: https://github.com/AndPuQing/iotop/compare/v0.1.8...HEAD
[0.1.8]: https://github.com/AndPuQing/iotop/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/AndPuQing/iotop/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/AndPuQing/iotop/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/AndPuQing/iotop/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/AndPuQing/iotop/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/AndPuQing/iotop/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/AndPuQing/iotop/compare/v0.1.0...v0.1.2
[0.1.0]: https://github.com/AndPuQing/iotop/releases/tag/v0.1.0
