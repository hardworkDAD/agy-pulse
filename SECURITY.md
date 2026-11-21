# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| `1.x` / `main` | :white_check_mark: Yes |
| `< 1.0` | :x: No |

## Local-First Security Model

`agy-pulse` is designed with a strict local-first, privacy-preserving architecture:

- **Read-Only Telemetry Scanning**: Local session transcripts and SQLite databases are scanned read-only to compute context, compaction, speed, and cost metrics.
- **Loopback-Only Web Dashboard**: `agypl dashboard` binds exclusively to `127.0.0.1` (loopback) and enforces `Host` / `Origin` checks along with Content Security Policy (CSP) headers.
- **No Third-Party Telemetry Uploads**: OAuth tokens, keyring secrets, and local conversation/session data are never uploaded to third-party servers. Network calls are limited to official Antigravity/cloud quota endpoints and explicit update checks.

## Vulnerability Reporting

If you discover a security vulnerability, please report it **privately** using GitHub Private Security Advisories:

- **Report here**: https://github.com/hardworkDAD/agy-pulse/security/advisories/new

> [!CAUTION]
> **Never paste plaintext OAuth tokens, keyring secrets, API keys, or unredacted logs into public GitHub Issues, Discussions, or Pull Requests.**

### What to Include

To help us reproduce and resolve the issue quickly, please include:

- Affected `agy-pulse` version / commit SHA, operating system, and Node.js version
- Clear description of the vulnerability and potential impact
- Step-by-step reproduction instructions or a minimal proof of concept
- Redacted diagnostic output (for example, `agypl diagnose` with any sensitive values removed)

### Response Process

1. **Acknowledgment**: Maintainers will review the advisory and acknowledge receipt as soon as possible.
2. **Investigation & Fix**: We will validate the report, determine affected versions, and prepare a fix privately.
3. **Coordinated Disclosure**: Once a patch is ready and released, we will publish the advisory details and credit the reporter (unless anonymity is requested).
