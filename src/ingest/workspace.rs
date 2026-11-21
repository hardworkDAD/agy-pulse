use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::platform;

/// Resolve the display basename of the workspace directory.
pub(super) fn resolve_current_dir_name(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

/// Resolve the current Git branch by reading `.git/HEAD` directly first (supporting both
/// normal repositories and worktree `.git` pointer files), falling back to `git rev-parse`
/// with a tight timeout only for complex repository formats (e.g., reftable).
pub(super) fn resolve_git_branch(cwd: &Path) -> Option<String> {
    let git_dir = locate_git_dir(cwd)?;

    if !git_dir.join("reftable").exists()
        && let Some(branch) = read_head_branch(&git_dir)
    {
        return Some(branch);
    }

    platform::run_command_with_timeout(
        "git",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        cwd,
        Duration::from_millis(300),
    )
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
        let short = other_ref
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim();
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
