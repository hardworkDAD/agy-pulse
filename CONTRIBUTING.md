# Contributing to agy-pulse

Thank you for your interest in contributing to **agy-pulse**! Please read this guide along with our [Code of Conduct](./CODE_OF_CONDUCT.md) and [Security Policy](./SECURITY.md) before submitting changes.

## Prerequisites

- **Rust stable toolchain** (with `clippy` component; Edition 2024, `Cargo.toml` manages all bundled dependencies including `rusqlite` with `bundled` SQLite).
- Zero Node.js or external database runtime dependencies are required.

## Development & Submission Workflow

1. **Fork & Branch**: Fork the repository and create a dedicated feature or fix branch from `main` (do not push directly to `main`).
2. **Develop & Verify**: Make your changes locally and run both Clippy and the full integration test suite:
   ```bash
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
3. **Commit Style**: Follow [Conventional Commits](https://www.conventionalcommits.org/) (for example: `feat: ...`, `fix: ...`, `docs: ...`, `refactor: ...`, `test: ...`, `chore: ...`).
4. **Pull Request**: Open a Pull Request against `main` and complete the checklist in [`.github/PULL_REQUEST_TEMPLATE.md`](./.github/PULL_REQUEST_TEMPLATE.md).

## Engineering Constraints

All contributions must preserve the core architectural invariants of `agy-pulse` v2.0.0:

- **Domain Terminology Alignment**: Follow [`GLOSSARY.md`](./GLOSSARY.md) strictly (`Statusline`, `Dashboard`, `Conversation`, `Cascade`, `Ledger`, `Compaction`, `Quota`).
- **Zero Off-Host Antigravity Requests (ADR-0002)**: Never add code that accesses Antigravity / Google cloud quota endpoints, reads system keychains, `oauth_creds.json`, or `google_accounts.json`, writes into official `agy` installation directories, or inspects `/proc/<ppid>`.
- **Deep-Module & Acyclic Boundaries (ADR-0003)**:
  - Keep `src/domain/` and `src/render/` **100% pure in-memory** (zero file I/O, network, child processes, or implicit clock reads).
  - Communicate between modules strictly via pure `domain` value structs; never leak `rusqlite::Connection` or file handles across module boundaries.
  - Isolate OS-specific conditional compilation (`#[cfg(unix)]` / `#[cfg(windows)]`) inside `src/platform/`.
- **Zero Panic Gate in Library Code**: `src/lib.rs` enforces `#![deny(clippy::unwrap_used, clippy::expect_used)]`. Handle errors explicitly with `Result` / `thiserror`.
- **Non-Blocking Statusline Hot Path**: The Statusline entrypoint operates under a **1500 ms** hard timeout, never blocks on network calls, and always exits `0`.
- **Isolated Black-Box Tests**: Integration tests under `tests/` must isolate state via `tempfile::TempDir` and environment variables (`AGY_PULSE_DATA_DIR`, `AGY_PULSE_IDE_DATA_DIR`, `AGY_PULSE_GLOBAL_CONFIG_DIR`, `AGY_PULSE_BIN_DIR`, `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`) and must **never** touch `~/.gemini` or `~/.local/bin`.
- **Bilingual Synchronization**: Keep user-facing documentation and UI copy synchronized between English and Simplified Chinese.

## Further Reading

- [Maintainer Docs Index](./docs/README.md)
- [Architecture Overview](./docs/architecture.md)
- [Domain Glossary](./GLOSSARY.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Security Policy](./SECURITY.md)
