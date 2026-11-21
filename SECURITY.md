# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| `2.x` / `main` | :white_check_mark: Yes |
| `< 2.0` | :x: No (EOL; retired Node.js runtime) |

## Local-First Security & Compliance Model

`agy-pulse` v2.0.0 enforces a strict local-first, privacy-preserving, zero-credential architecture:

- **Zero Credential Access & Zero Off-Host Antigravity Requests (ADR-0002)**: `agy-pulse` never accesses system keychains, Windows Credential Manager, `oauth_creds.json`, or `google_accounts.json`, and never initiates requests to Antigravity or model provider servers. All **Quota** and tier displays consume data provided directly by the host over `stdin`.
- **Lock-Free Read-Only Telemetry Scanning**: Host conversation logs and SQLite databases are scanned read-only (`?immutable=1` for archived databases or temporary copies for active `-wal` databases) with zero writes to host conversation directories.
- **Loopback-Only Web Dashboard**: `agypl dashboard` binds exclusively to `127.0.0.1` (loopback), validates `Host` and `Origin` headers, and serves Content Security Policy (CSP) headers with zero external CDN dependencies.
- **Verified Self-Update & Safe Symlink Handling**: `agypl update` and installer scripts allow only `https://` URLs (or `http://` on loopback for local testing), forbid `https:// -> http://` redirect downgrades, and enforce `SHA256SUMS` verification before replacing binaries. Error logs (`agy-pulse-error.log`) check for symlinks before writing (`0600`).

## Vulnerability Reporting

If you discover a security vulnerability, please report it **privately** using GitHub Private Security Advisories:

- **Report here**: https://github.com/hardworkDAD/agy-pulse/security/advisories/new

> [!CAUTION]
> **Never paste personal conversation transcripts, SQLite databases, API keys, or unredacted local paths into public GitHub Issues, Discussions, or Pull Requests.**

### What to Include

To help us reproduce and resolve the issue quickly, please include:

- Affected `agy-pulse` version / commit SHA, operating system, and CPU architecture
- Clear description of the vulnerability and potential impact
- Step-by-step reproduction instructions or a minimal proof of concept

### Response Process

1. **Acknowledgment**: Maintainers will review the advisory and acknowledge receipt as soon as possible.
2. **Investigation & Fix**: We will validate the report, determine affected versions, and prepare a fix privately.
3. **Coordinated Disclosure**: Once a patch is ready and released, we will publish the advisory details and credit the reporter (unless anonymity is requested).
