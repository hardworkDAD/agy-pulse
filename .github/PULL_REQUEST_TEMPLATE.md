## Summary

<!-- Briefly describe what this Pull Request changes and why. -->

## Related Issue

<!-- Link related issue(s), e.g., Closes #123 -->

## Type of Change

- [ ] Bug fix (`fix`)
- [ ] New feature (`feat`)
- [ ] Documentation update (`docs`)
- [ ] Refactor / performance improvement (`refactor` / `perf`)
- [ ] Tests / maintenance (`test` / `chore`)

## Verification & Checklist

- [ ] `cargo clippy --all-targets -- -D warnings` passes with zero warnings
- [ ] `cargo test` passes completely with zero failures
- [ ] Zero off-host Antigravity / Google cloud requests or credential reads introduced (ADR-0002)
- [ ] User-facing documentation and UI strings are synchronized in English and Simplified Chinese (or N/A)
- [ ] No sensitive credentials, OAuth tokens, or unredacted personal logs are included
