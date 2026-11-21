mod stdin;
mod workspace;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::PulseSnapshot;

/// Result of reading host standard input and local workspace metadata.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub snapshot: PulseSnapshot,
    pub workspace_dir: PathBuf,
    pub diagnostics: Vec<String>,
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Narrow facade for reading host inputs and workspace state.
pub struct HostReader;

impl HostReader {
    /// Parse host stdin payload and enrich with local workspace directory and Git branch.
    pub fn read_live_pulse(stdin_text: &str, fallback_cwd: &Path) -> IngestResult {
        let observed_at_ms = current_epoch_ms();
        let mut parsed = stdin::parse_host_stdin(stdin_text, fallback_cwd, observed_at_ms);
        parsed.snapshot.workspace.current_dir =
            workspace::resolve_current_dir_name(&parsed.workspace_dir);
        parsed.snapshot.workspace.git_branch =
            workspace::resolve_git_branch(&parsed.workspace_dir);
        parsed
    }
}
