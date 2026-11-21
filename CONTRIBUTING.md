# Contributing to agy-pulse

Thank you for your interest in contributing to **agy-pulse**! Please read this guide along with our [Code of Conduct](./CODE_OF_CONDUCT.md) and [Security Policy](./SECURITY.md) before submitting changes.

## Prerequisites

- **Node.js >= 22.5** (Node.js 24 LTS recommended for built-in `node:sqlite` and running the full test suite; runtime degrades gracefully to JSON ledger on Node 20)
- **Zero external npm dependencies**: there is no `npm install` step; everything runs on Node.js built-in modules.

## Development & Submission Workflow

1. **Fork & Branch**: Fork the repository and create a dedicated feature or fix branch from `main` (do not push directly to `main`).
2. **Develop & Test**: Make your changes locally and run `npm test` to verify all unit tests pass.
3. **Commit Style**: Follow [Conventional Commits](https://www.conventionalcommits.org/) (for example: `feat: ...`, `fix: ...`, `docs: ...`, `refactor: ...`, `test: ...`, `chore: ...`).
4. **Pull Request**: Open a Pull Request against `main` and complete the checklist in [`.github/PULL_REQUEST_TEMPLATE.md`](./.github/PULL_REQUEST_TEMPLATE.md).

## Engineering Constraints

All contributions must preserve the core architectural invariants of `agy-pulse`:

- **Zero Third-Party Dependencies**: Use Node.js built-in modules only. Do not add external packages to `package.json`.
- **Non-Blocking Status Line Hot Path**: The status line entry point (`runtime/bin/agy-pulse.js`) operates under a strict **1.5s hard budget** and must never block on network requests. Cloud quota refreshes run via background Stale-While-Revalidate (SWR).
- **Isolated Unit Tests**: All unit tests must use `node:test` and `node:assert/strict` with temporary directories and environment variable isolation (such as `AGY_PULSE_DATA_DIR`). Tests must **never** read from or write to the host `~/.gemini` directory.
- **Runtime Manifest Synchronization**: Any newly added file under `runtime/` must also be registered in both `scripts/bootstrap.js` (`RUNTIME_FILES`) and `scripts/deploy-local.sh`.
- **Bilingual Synchronization**: Keep user-facing documentation and UI copy synchronized between English and Simplified Chinese.
- **Pre-Submit Verification**: `npm test` must pass cleanly before submitting a Pull Request.

## Further Reading

- [Maintainer Docs Index](./docs/README.md)
- [Architecture Overview](./docs/architecture.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Security Policy](./SECURITY.md)
