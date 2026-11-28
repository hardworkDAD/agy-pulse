use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::domain::WorkspaceMetadata;
use crate::platform;

/// Resolve the display basename of the workspace directory.
pub(super) fn resolve_current_dir_name(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

fn remaining_git_timeout(deadline: Option<Instant>) -> Option<Duration> {
    match deadline {
        Some(d) => d
            .checked_duration_since(Instant::now())
            .filter(|rem| !rem.is_zero())
            .map(|rem| rem.min(Duration::from_millis(300))),
        None => Some(Duration::from_millis(300)),
    }
}

/// Resolve the current Git branch by reading `.git/HEAD` directly first (supporting both
/// normal repositories and worktree `.git` pointer files), falling back to `git rev-parse`
/// with a tight timeout only for complex repository formats (e.g., reftable).
pub(super) fn resolve_git_branch(cwd: &Path, deadline: Option<Instant>) -> Option<String> {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return None;
    }
    let git_dir = locate_git_dir(cwd)?;

    if !git_dir.join("reftable").exists()
        && let Some(branch) = read_head_branch(&git_dir)
    {
        return Some(branch);
    }

    let timeout = remaining_git_timeout(deadline)?;
    platform::run_command_with_timeout("git", &["rev-parse", "--abbrev-ref", "HEAD"], cwd, timeout)
}

fn locate_git_dir(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    let mut depth = 0usize;
    while let Some(dir) = current {
        if depth >= 32 {
            break;
        }
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            let content = fs::read_to_string(&dot_git).ok()?;
            for line in content.lines() {
                if let Some(raw_target) = line.trim().strip_prefix("gitdir:") {
                    let target = raw_target.trim();
                    if !target.is_empty() {
                        let candidate = PathBuf::from(target);
                        return Some(if candidate.is_absolute() {
                            candidate
                        } else {
                            dir.join(candidate)
                        });
                    }
                }
            }
            return None;
        }
        current = dir.parent();
        depth += 1;
    }
    None
}

fn read_head_branch(git_dir: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let trimmed = head.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        let branch = branch.trim();
        return if branch.is_empty() {
            None
        } else {
            Some(branch.to_string())
        };
    }
    if let Some(other_ref) = trimmed.strip_prefix("ref: ") {
        let short = other_ref.trim().rsplit('/').next().unwrap_or("").trim();
        return if short.is_empty() {
            None
        } else {
            Some(short.to_string())
        };
    }
    if (trimmed.len() == 40 || trimmed.len() == 64)
        && trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Some("HEAD".to_string());
    }
    None
}

/// Pure offline scan of workspace memory files, rule documents, git hooks, and configured MCP servers.
pub(super) fn scan_workspace_metadata(cwd: &Path, deadline: Option<Instant>) -> WorkspaceMetadata {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return WorkspaceMetadata::default();
    }
    let memory_file = detect_memory_file(cwd);
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return WorkspaceMetadata {
            memory_file,
            ..Default::default()
        };
    }
    let rules_count = count_rule_documents(cwd, deadline);
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return WorkspaceMetadata {
            memory_file,
            rules_count,
            ..Default::default()
        };
    }
    let hooks_count = count_git_hooks(cwd, deadline);
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return WorkspaceMetadata {
            memory_file,
            rules_count,
            hooks_count,
            ..Default::default()
        };
    }
    let mcp_count = count_mcp_servers(deadline);

    WorkspaceMetadata {
        memory_file,
        breadcrumbs: Vec::new(),
        rules_count,
        mcp_count,
        hooks_count,
    }
}

fn detect_memory_file(cwd: &Path) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "AGENTS.md",
        "agents.md",
        "GEMINI.md",
        "gemini.md",
        "CLAUDE.md",
        "claude.md",
        "MEMORY.md",
        "memory.md",
    ];

    if let Ok(rd) = fs::read_dir(cwd) {
        let names: HashSet<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        for &candidate in CANDIDATES {
            if names.contains(candidate) && cwd.join(candidate).is_file() {
                return Some(candidate.to_string());
            }
        }
    } else {
        for &candidate in CANDIDATES {
            if cwd.join(candidate).is_file() {
                return Some(candidate.to_string());
            }
        }
    }

    if let Some(home) = platform::home_dir() {
        let normalized = cwd.to_string_lossy().replace('\\', "/");
        let project_key = normalized.replace(':', "").replace('/', "-");
        let project_memory = home
            .join(".claude")
            .join("projects")
            .join(project_key)
            .join("memory")
            .join("MEMORY.md");
        if project_memory.is_file() {
            return Some("MEMORY.md".to_string());
        }
    }

    None
}

fn count_md_files_in_dir(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0u64;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md")
            && entry.file_type().is_ok_and(|ft| ft.is_file())
        {
            count += 1;
        }
    }
    count
}

fn count_rule_documents(cwd: &Path, deadline: Option<Instant>) -> u64 {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return 0;
    }
    let mut dirs: Vec<PathBuf> = vec![
        cwd.join(".claude").join("rules"),
        cwd.join(".cursor").join("rules"),
        cwd.join(".github").join("rules"),
        cwd.join(".gemini").join("rules"),
        cwd.join(".antigravity").join("rules"),
    ];
    for root in platform::get_antigravity_roots() {
        dirs.push(root.join("rules"));
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut total = 0u64;
    for dir in dirs {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if !dir.is_dir() {
            continue;
        }
        let key = fs::canonicalize(&dir).unwrap_or(dir);
        if seen.insert(key.clone()) {
            total += count_md_files_in_dir(&key);
        }
    }
    total
}

fn resolve_common_git_dir(git_dir: &Path) -> Option<PathBuf> {
    let raw = fs::read_to_string(git_dir.join("commondir")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    Some(if p.is_absolute() { p } else { git_dir.join(p) })
}

fn count_git_hooks(cwd: &Path, deadline: Option<Instant>) -> u64 {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return 0;
    }
    let Some(git_dir) = locate_git_dir(cwd) else {
        return 0;
    };
    let mut hooks_dir = git_dir.join("hooks");
    if !hooks_dir.is_dir()
        && let Some(common) = resolve_common_git_dir(&git_dir)
    {
        hooks_dir = common.join("hooks");
    }
    if !hooks_dir.is_dir()
        && let Some(timeout) = remaining_git_timeout(deadline)
        && let Some(raw_path) = platform::run_command_with_timeout(
            "git",
            &["rev-parse", "--git-path", "hooks"],
            cwd,
            timeout,
        )
    {
        let p = PathBuf::from(raw_path);
        hooks_dir = if p.is_absolute() { p } else { cwd.join(p) };
    }

    let Ok(rd) = fs::read_dir(&hooks_dir) else {
        return 0;
    };
    let mut count = 0u64;
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".sample") || name_str.ends_with(".disabled") {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.is_file()) {
            count += 1;
        }
    }
    count
}

fn count_mcp_entries_in_file(path: &Path) -> u64 {
    let Ok(raw) = fs::read_to_string(path) else {
        return 0;
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return 0;
    };
    let Some(mcps) = val.get("mcpServers").or_else(|| val.get("mcp")) else {
        return 0;
    };
    mcps.as_object().map(|m| m.len() as u64).unwrap_or(0)
}

fn count_mcp_servers(deadline: Option<Instant>) -> u64 {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return 0;
    }
    let mut candidates = vec![
        platform::resolve_antigravity_path("settings.json"),
        platform::resolve_antigravity_path("mcp_config.json"),
    ];
    if let Some(claude_cfg) = platform::resolve_claude_desktop_config_path() {
        candidates.push(claude_cfg);
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut total = 0u64;
    for path in candidates {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if !path.is_file() {
            continue;
        }
        let key = fs::canonicalize(&path).unwrap_or(path);
        if seen.insert(key.clone()) {
            total += count_mcp_entries_in_file(&key);
        }
    }
    total
}
