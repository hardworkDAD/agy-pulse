use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::platform;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Init = 0,
    Stdin = 1,
    IngestParse = 2,
    IngestTranscript = 3,
    IngestSpeed = 4,
    IngestCompaction = 5,
    IngestDbPerf = 6,
    IngestCascade = 7,
    IngestWorkspace = 8,
    Config = 9,
    DashboardResolve = 10,
    UpdateCheck = 11,
    LedgerOpen = 12,
    LedgerBgSyncSpawn = 13,
    LedgerSyncConv = 14,
    LedgerRolling = 15,
    Render = 16,
    DashboardTouch = 17,
    WriteStdout = 18,
    WriteProfileLog = 19,
    Done = 20,
}

impl Stage {
    fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Stdin,
            2 => Self::IngestParse,
            3 => Self::IngestTranscript,
            4 => Self::IngestSpeed,
            5 => Self::IngestCompaction,
            6 => Self::IngestDbPerf,
            7 => Self::IngestCascade,
            8 => Self::IngestWorkspace,
            9 => Self::Config,
            10 => Self::DashboardResolve,
            11 => Self::UpdateCheck,
            12 => Self::LedgerOpen,
            13 => Self::LedgerBgSyncSpawn,
            14 => Self::LedgerSyncConv,
            15 => Self::LedgerRolling,
            16 => Self::Render,
            17 => Self::DashboardTouch,
            18 => Self::WriteStdout,
            19 => Self::WriteProfileLog,
            20 => Self::Done,
            _ => Self::Init,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Stdin => "stdin",
            Self::IngestParse => "ingest_parse",
            Self::IngestTranscript => "ingest_transcript",
            Self::IngestSpeed => "ingest_speed",
            Self::IngestCompaction => "ingest_compaction",
            Self::IngestDbPerf => "ingest_db_perf",
            Self::IngestCascade => "ingest_cascade",
            Self::IngestWorkspace => "ingest_workspace",
            Self::Config => "config",
            Self::DashboardResolve => "dashboard_resolve",
            Self::UpdateCheck => "update_check",
            Self::LedgerOpen => "ledger_open",
            Self::LedgerBgSyncSpawn => "ledger_bg_sync_spawn",
            Self::LedgerSyncConv => "ledger_sync_conv",
            Self::LedgerRolling => "ledger_rolling",
            Self::Render => "render",
            Self::DashboardTouch => "dashboard_touch",
            Self::WriteStdout => "stdout",
            Self::WriteProfileLog => "profile_log",
            Self::Done => "done",
        }
    }
}

thread_local! {
    static ACTIVE_STAGE: RefCell<Option<Arc<AtomicU8>>> = const { RefCell::new(None) };
}

pub(crate) struct StageScopeGuard {
    prev: Option<Arc<AtomicU8>>,
}

impl Drop for StageScopeGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        let _ = ACTIVE_STAGE.try_with(|cell| {
            *cell.borrow_mut() = prev;
        });
    }
}

pub(crate) fn enter_stage_scope(stage: Arc<AtomicU8>) -> StageScopeGuard {
    let prev = ACTIVE_STAGE.with(|cell| cell.borrow_mut().replace(stage));
    StageScopeGuard { prev }
}

pub(crate) fn set_stage(stage: Stage) {
    ACTIVE_STAGE.with(|cell| {
        if let Some(ref active) = *cell.borrow() {
            // A worker that outlives the fallback must not overwrite the output stage.
            let stage = stage as u8;
            let first_output_stage = Stage::WriteStdout as u8;
            let _ = active.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (stage >= first_output_stage || current < first_output_stage).then_some(stage)
            });
        }
    });
}

fn append_line(path: &Path, line: &str) {
    platform::append_file_0600_no_symlink(path, line);
}

#[derive(Debug, Clone, Default)]
pub struct IngestStageTimings {
    pub parse_ms: u64,
    pub transcript_ms: u64,
    pub speed_ms: u64,
    pub compaction_ms: u64,
    pub db_perf_ms: u64,
    pub cascade_ms: u64,
    pub workspace_ms: u64,
}

pub(crate) struct Profiler {
    log_path: Option<PathBuf>,
    min_ms: u64,
    started: Instant,
    done: Arc<AtomicBool>,
    stage: Arc<AtomicU8>,
}

impl Profiler {
    pub(crate) fn start_from_env() -> Self {
        let log_path = env::var_os("AGY_PULSE_PROFILE_LOG")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        let min_ms = env::var("AGY_PULSE_PROFILE_MIN_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(200);
        let started = Instant::now();
        let done = Arc::new(AtomicBool::new(false));
        let stage = Arc::new(AtomicU8::new(Stage::Init as u8));

        if let Some(ref path) = log_path {
            let watch_path = path.clone();
            let watch_done = Arc::clone(&done);
            let watch_stage = Arc::clone(&stage);
            let pid = std::process::id();
            let _ = thread::Builder::new()
                .name("agy-pulse-prof".to_string())
                .spawn(move || {
                    const CHECKPOINTS_MS: &[u64] = &[800, 2000, 3500];
                    for &cp in CHECKPOINTS_MS {
                        loop {
                            if watch_done.load(Ordering::Acquire) {
                                return;
                            }
                            let elapsed = started.elapsed().as_millis() as u64;
                            if elapsed >= cp {
                                let ts = chrono::Local::now().format("%H:%M:%S%.3f");
                                let cur = Stage::from_u8(watch_stage.load(Ordering::Acquire)).as_str();
                                let line = format!(
                                    "[{ts}] pid={pid} STALL elapsed={elapsed}ms stage={cur}\n"
                                );
                                append_line(&watch_path, &line);
                                break;
                            }
                            let remaining = cp - elapsed;
                            thread::sleep(Duration::from_millis(remaining.min(100)));
                        }
                    }
                });
        }

        Self {
            log_path,
            min_ms,
            started,
            done,
            stage,
        }
    }

    pub(crate) fn stage_handle(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.stage)
    }

    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub(crate) fn finish(&self, summary: &str) {
        let total = self.elapsed_ms();
        if let Some(path) = &self.log_path
            && total >= self.min_ms
        {
            self.stage
                .store(Stage::WriteProfileLog as u8, Ordering::Release);
            let ts = chrono::Local::now().format("%H:%M:%S%.3f");
            let pid = std::process::id();
            let line = format!("[{ts}] pid={pid} DONE total={total}ms {summary}\n");
            append_line(path, &line);
        }
        self.stage.store(Stage::Done as u8, Ordering::Release);
        self.done.store(true, Ordering::Release);
    }
}

impl Drop for Profiler {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}
