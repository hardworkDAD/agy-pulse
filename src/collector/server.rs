use std::collections::{HashMap, VecDeque};
use std::env;
#[cfg(unix)]
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::platform;

use super::protocol::{
    COLLECTOR_APP_ID, CollectorReadyPayload, CollectorRequest, CollectorStateFile,
    CollectorWireRequest, CollectorWireResponse, IpcStream, clear_collector_state_if_owner,
    exe_mtime_matches, read_collector_state, read_frame, release_collector_spawn_lock,
    send_shutdown_to_state, write_collector_state, write_frame,
};
use super::snapshot::write_snapshots_and_prune;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(15);
const STREAM_IO_TIMEOUT: Duration = Duration::from_millis(200);
const BUSY_OVERLOAD_WRITE_TIMEOUT: Duration = Duration::from_millis(50);
const DATA_DIR_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const TAKEOVER_WAIT_TIMEOUT: Duration = Duration::from_millis(500);
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_CONCURRENT_WAITERS: usize = 16;
const MAX_CONCURRENT_IPC_HANDLERS: usize = 8;
const MAX_PENDING_JOBS: usize = 64;
const DEFAULT_PROD_IDLE_SEC: u64 = 300;
const DEFAULT_TEST_DATA_DIR_IDLE_SEC: u64 = 2;

static EMBEDDED_COLLECTOR_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Return `true` while an embedded collector server is active in the current process.
pub fn is_embedded_collector_active_in_process() -> bool {
    EMBEDDED_COLLECTOR_ACTIVE.load(Ordering::Acquire)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectorMode {
    Headless,
    Embedded,
}

impl CollectorMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "collector",
            Self::Embedded => "dashboard",
        }
    }
}

struct PendingJob {
    req: CollectorRequest,
    waiters: Vec<SyncSender<CollectorWireResponse>>,
}

struct CollectorQueueState {
    worker_busy: bool,
    max_pending_jobs: usize,
    pending_order: VecDeque<String>,
    pending_jobs: HashMap<String, PendingJob>,
    shutdown: bool,
    last_activity: Instant,
    last_env_overrides: Option<HashMap<String, Option<String>>>,
}

impl CollectorQueueState {
    fn with_capacity(max_pending_jobs: usize) -> Self {
        Self {
            worker_busy: false,
            max_pending_jobs: max_pending_jobs.max(1),
            pending_order: VecDeque::new(),
            pending_jobs: HashMap::new(),
            shutdown: false,
            last_activity: Instant::now(),
            last_env_overrides: None,
        }
    }

    fn can_accept(&self, conversation_key: &str) -> bool {
        self.pending_jobs.contains_key(conversation_key)
            || self.pending_jobs.len() < self.max_pending_jobs
    }

    fn enqueue(
        &mut self,
        req: CollectorRequest,
        waiter: Option<SyncSender<CollectorWireResponse>>,
    ) -> bool {
        let key = req.conversation_key.clone();
        if let Some(existing) = self.pending_jobs.get_mut(&key) {
            self.last_env_overrides = Some(req.env_overrides.clone());
            existing.req = req;
            if let Some(tx) = waiter {
                existing.waiters.push(tx);
            }
            return true;
        }
        if self.pending_jobs.len() >= self.max_pending_jobs {
            return false;
        }
        self.last_env_overrides = Some(req.env_overrides.clone());
        let mut waiters = Vec::new();
        if let Some(tx) = waiter {
            waiters.push(tx);
        }
        self.pending_order.push_back(key.clone());
        self.pending_jobs.insert(key, PendingJob { req, waiters });
        true
    }
}

struct AtomicSlotGuard(Arc<AtomicUsize>);

impl AtomicSlotGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        let mut cur = counter.load(Ordering::Acquire);
        loop {
            if cur >= limit {
                return None;
            }
            match counter.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(Arc::clone(counter))),
                Err(actual) => cur = actual,
            }
        }
    }
}

impl Drop for AtomicSlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

enum IpcListener {
    #[cfg(unix)]
    Unix {
        listener: std::os::unix::net::UnixListener,
        socket_path: PathBuf,
    },
    #[cfg(windows)]
    Tcp {
        listener: std::net::TcpListener,
        port: u16,
    },
}

impl IpcListener {
    fn bind() -> io::Result<Self> {
        #[cfg(unix)]
        {
            let socket_path = platform::resolve_ipc_socket_path();
            if let Some(parent) = socket_path.parent() {
                platform::ensure_dir_0700(parent)?;
            }
            if fs::symlink_metadata(&socket_path).is_ok() {
                let _ = fs::remove_file(&socket_path);
            }
            let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
            let _ = platform::set_file_0600(&socket_path);
            listener.set_nonblocking(true)?;
            Ok(Self::Unix {
                listener,
                socket_path,
            })
        }
        #[cfg(windows)]
        {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port();
            listener.set_nonblocking(true)?;
            Ok(Self::Tcp { listener, port })
        }
    }

    fn accept(&self) -> io::Result<IpcStream> {
        match self {
            #[cfg(unix)]
            Self::Unix { listener, .. } => {
                let (stream, _) = listener.accept()?;
                Ok(IpcStream::Unix(stream))
            }
            #[cfg(windows)]
            Self::Tcp { listener, .. } => {
                let (stream, _) = listener.accept()?;
                let _ = stream.set_nodelay(true);
                Ok(IpcStream::Tcp(stream))
            }
        }
    }

    fn socket_path_str(&self) -> Option<String> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket_path, .. } => Some(socket_path.to_string_lossy().into_owned()),
            #[cfg(windows)]
            Self::Tcp { .. } => None,
        }
    }

    fn tcp_port(&self) -> Option<u16> {
        match self {
            #[cfg(unix)]
            Self::Unix { .. } => None,
            #[cfg(windows)]
            Self::Tcp { port, .. } => Some(*port),
        }
    }

    fn cleanup_socket_file(&self) {
        #[cfg(unix)]
        {
            let Self::Unix { socket_path, .. } = self;
            let _ = fs::remove_file(socket_path);
        }
    }
}

/// Handle managing the embedded collector threads inside the unified dashboard daemon.
pub struct CollectorServerHandle {
    shutdown_flag: Arc<AtomicBool>,
    queue: Arc<(Mutex<CollectorQueueState>, Condvar)>,
    worker_done: Arc<(Mutex<bool>, Condvar)>,
    ipc_thread: Option<JoinHandle<()>>,
    worker_thread: Option<JoinHandle<()>>,
}

fn wait_for_flag(state: &(Mutex<bool>, Condvar), timeout: Duration) -> bool {
    let (lock, cvar) = state;
    let Ok(guard) = lock.lock() else {
        return false;
    };
    cvar.wait_timeout_while(guard, timeout, |done| !*done)
        .map(|(g, _)| *g)
        .unwrap_or(false)
}

impl CollectorServerHandle {
    /// Gracefully shut down any standalone headless collector and start the embedded collector
    /// sharing lifecycle and keepalive activity with the unified dashboard server.
    pub fn start_embedded(
        external_shutdown: Arc<AtomicBool>,
        on_activity: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        if let Some(existing) = read_collector_state()
            && existing.app == COLLECTOR_APP_ID
            && existing.pid != std::process::id()
            && platform::is_process_alive(existing.pid)
        {
            takeover_existing_collector(&existing);
        }

        let handle = start_collector_internal(
            CollectorMode::Embedded,
            None,
            Some(external_shutdown),
            Some(on_activity),
            None,
        )?;
        EMBEDDED_COLLECTOR_ACTIVE.store(true, Ordering::Release);
        Ok(handle)
    }

    /// Signal the collector IPC and worker threads to stop and join them with a bounded worker wait.
    pub fn shutdown(&mut self) {
        EMBEDDED_COLLECTOR_ACTIVE.store(false, Ordering::Release);
        self.shutdown_flag.store(true, Ordering::Release);
        {
            let (lock, cvar) = &*self.queue;
            if let Ok(mut guard) = lock.lock() {
                guard.shutdown = true;
                cvar.notify_all();
            }
        }
        if let Some(handle) = self.ipc_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.worker_thread.take() {
            if wait_for_flag(&self.worker_done, WORKER_SHUTDOWN_TIMEOUT) {
                let _ = handle.join();
            } else {
                platform::write_error_log_safely(
                    "Collector worker thread did not finish within 1500ms during shutdown; detaching",
                );
            }
        }
    }
}

impl Drop for CollectorServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn takeover_existing_collector(existing: &CollectorStateFile) {
    let acked = send_shutdown_to_state(existing, Duration::from_millis(200));
    let is_embedded = existing.mode == CollectorMode::Embedded.as_str();
    let deadline = Instant::now() + TAKEOVER_WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if !platform::is_process_alive(existing.pid) {
            break;
        }
        if acked
            && is_embedded
            && read_collector_state()
                .as_ref()
                .is_none_or(|s| s.token != existing.token)
        {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
}

fn is_same_collector_binary(existing: &CollectorStateFile, my_exe_mtime: i64) -> bool {
    existing.app == COLLECTOR_APP_ID
        && existing.version == crate::app::VERSION
        && exe_mtime_matches(existing.exe_mtime_ms, my_exe_mtime)
}

fn start_collector_internal(
    mode: CollectorMode,
    idle_timeout: Option<Duration>,
    external_shutdown: Option<Arc<AtomicBool>>,
    on_activity: Option<Arc<dyn Fn() + Send + Sync>>,
    watch_data_dir: Option<PathBuf>,
) -> io::Result<CollectorServerHandle> {
    let listener = IpcListener::bind()?;
    let owner_pid = std::process::id();
    let token = platform::random_128bit_hex();
    let state = CollectorStateFile {
        app: COLLECTOR_APP_ID.to_string(),
        pid: owner_pid,
        version: crate::app::VERSION.to_string(),
        exe_mtime_ms: platform::current_exe_mtime_ms(),
        mode: mode.as_str().to_string(),
        socket_path: listener.socket_path_str(),
        tcp_port: listener.tcp_port(),
        token: token.clone(),
        started_at_ms: current_epoch_ms(),
    };
    if let Err(err) = write_collector_state(&state) {
        listener.cleanup_socket_file();
        return Err(err);
    }

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let queue = Arc::new((
        Mutex::new(CollectorQueueState::with_capacity(MAX_PENDING_JOBS)),
        Condvar::new(),
    ));
    let worker_done = Arc::new((Mutex::new(false), Condvar::new()));

    let worker_queue = Arc::clone(&queue);
    let worker_shutdown = Arc::clone(&shutdown_flag);
    let worker_done_flag = Arc::clone(&worker_done);
    let worker_thread = thread::Builder::new()
        .name("agy-pulse-collector-worker".to_string())
        .spawn(move || run_worker_loop(worker_queue, worker_shutdown, worker_done_flag))?;

    let ipc_queue = Arc::clone(&queue);
    let ipc_shutdown = Arc::clone(&shutdown_flag);
    let ipc_thread = thread::Builder::new()
        .name("agy-pulse-collector-ipc".to_string())
        .spawn(move || {
            run_ipc_loop(
                listener,
                mode,
                owner_pid,
                token,
                idle_timeout,
                external_shutdown,
                on_activity,
                watch_data_dir,
                ipc_queue,
                ipc_shutdown,
            );
        })?;

    Ok(CollectorServerHandle {
        shutdown_flag,
        queue,
        worker_done,
        ipc_thread: Some(ipc_thread),
        worker_thread: Some(worker_thread),
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_ipc_connection(
    mut stream: IpcStream,
    ipc_guard: AtomicSlotGuard,
    mode: CollectorMode,
    owner_pid: u32,
    token: &str,
    on_activity: Option<&Arc<dyn Fn() + Send + Sync>>,
    queue: &(Mutex<CollectorQueueState>, Condvar),
    shutdown_flag: &AtomicBool,
    active_waiters: &Arc<AtomicUsize>,
) {
    let _ = stream.set_read_timeout(Some(STREAM_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(STREAM_IO_TIMEOUT));
    let req: CollectorWireRequest = match read_frame(&mut stream) {
        Ok(r) => r,
        Err(_) => return,
    };
    if req.token() != token {
        let _ = write_frame(
            &mut stream,
            &CollectorWireResponse::Error("unauthorized".to_string()),
        );
        return;
    }

    match req {
        CollectorWireRequest::Ping { .. } => {
            let busy = queue
                .0
                .lock()
                .map(|g| g.worker_busy)
                .unwrap_or(false);
            let _ = write_frame(
                &mut stream,
                &CollectorWireResponse::Pong {
                    pid: owner_pid,
                    version: crate::app::VERSION.to_string(),
                    mode: mode.as_str().to_string(),
                    worker_busy: busy,
                },
            );
        }
        CollectorWireRequest::Shutdown { .. } => {
            shutdown_flag.store(true, Ordering::Release);
            let (lock, cvar) = queue;
            if let Ok(mut guard) = lock.lock() {
                guard.shutdown = true;
                cvar.notify_all();
            }
            let _ = write_frame(&mut stream, &CollectorWireResponse::Ack);
        }
        CollectorWireRequest::Collect(collect_req) => {
            if let Some(cb) = on_activity {
                cb();
            }
            let wait_ms = collect_req.wait_ms;
            enum EnqueueOutcome {
                ReplyBusy,
                ReplyError(&'static str),
                Wait(mpsc::Receiver<CollectorWireResponse>, AtomicSlotGuard),
            }
            let outcome = {
                let (lock, cvar) = queue;
                match lock.lock() {
                    Err(_) => EnqueueOutcome::ReplyError("queue poisoned"),
                    Ok(mut guard) => {
                        guard.last_activity = Instant::now();
                        if guard.shutdown {
                            EnqueueOutcome::ReplyError("shutting down")
                        } else if !guard.can_accept(&collect_req.conversation_key) {
                            EnqueueOutcome::ReplyBusy
                        } else if (guard.worker_busy && collect_req.has_warm_snapshot)
                            || wait_ms == 0
                        {
                            if guard.enqueue(collect_req, None) {
                                cvar.notify_one();
                            }
                            EnqueueOutcome::ReplyBusy
                        } else if let Some(waiter_guard) =
                            AtomicSlotGuard::try_acquire(active_waiters, MAX_CONCURRENT_WAITERS)
                        {
                            let (tx, rx) = mpsc::sync_channel::<CollectorWireResponse>(1);
                            if guard.enqueue(collect_req, Some(tx)) {
                                cvar.notify_one();
                                EnqueueOutcome::Wait(rx, waiter_guard)
                            } else {
                                EnqueueOutcome::ReplyBusy
                            }
                        } else {
                            if guard.enqueue(collect_req, None) {
                                cvar.notify_one();
                            }
                            EnqueueOutcome::ReplyBusy
                        }
                    }
                }
            };

            // Release the IPC read slot before blocking for worker completion so slow/waiting
            // Collect calls do not starve new Ping/Shutdown/Collect connections.
            drop(ipc_guard);

            match outcome {
                EnqueueOutcome::ReplyError(msg) => {
                    let _ = write_frame(
                        &mut stream,
                        &CollectorWireResponse::Error(msg.to_string()),
                    );
                }
                EnqueueOutcome::ReplyBusy => {
                    let _ = write_frame(&mut stream, &CollectorWireResponse::Busy);
                }
                EnqueueOutcome::Wait(rx, _waiter_guard) => {
                    let resp = rx
                        .recv_timeout(Duration::from_millis(wait_ms))
                        .unwrap_or(CollectorWireResponse::Busy);
                    let _ = write_frame(&mut stream, &resp);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_ipc_loop(
    listener: IpcListener,
    mode: CollectorMode,
    owner_pid: u32,
    token: String,
    idle_timeout: Option<Duration>,
    external_shutdown: Option<Arc<AtomicBool>>,
    on_activity: Option<Arc<dyn Fn() + Send + Sync>>,
    watch_data_dir: Option<PathBuf>,
    queue: Arc<(Mutex<CollectorQueueState>, Condvar)>,
    shutdown_flag: Arc<AtomicBool>,
) {
    let active_waiters = Arc::new(AtomicUsize::new(0));
    let active_ipc_handlers = Arc::new(AtomicUsize::new(0));
    let mut last_dir_check = Instant::now();

    loop {
        if shutdown_flag.load(Ordering::Acquire)
            || external_shutdown
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Acquire))
            || platform::shutdown_signal_requested()
        {
            break;
        }

        if let Some(ref dir) = watch_data_dir
            && last_dir_check.elapsed() >= DATA_DIR_CHECK_INTERVAL
        {
            last_dir_check = Instant::now();
            if !dir.exists() {
                break;
            }
        }

        if let Some(idle_dur) = idle_timeout {
            let (lock, _) = &*queue;
            if let Ok(guard) = lock.lock()
                && !guard.worker_busy
                && guard.pending_order.is_empty()
                && guard.last_activity.elapsed() >= idle_dur
            {
                break;
            }
        }

        match listener.accept() {
            Ok(mut stream) => {
                let Some(ipc_guard) =
                    AtomicSlotGuard::try_acquire(&active_ipc_handlers, MAX_CONCURRENT_IPC_HANDLERS)
                else {
                    let _ = stream.set_write_timeout(Some(BUSY_OVERLOAD_WRITE_TIMEOUT));
                    let _ = write_frame(&mut stream, &CollectorWireResponse::Busy);
                    continue;
                };
                let conn_token = token.clone();
                let conn_on_activity = on_activity.clone();
                let conn_queue = Arc::clone(&queue);
                let conn_shutdown = Arc::clone(&shutdown_flag);
                let conn_waiters = Arc::clone(&active_waiters);
                let spawn_res = thread::Builder::new()
                    .name("agy-pulse-collector-ipc-conn".to_string())
                    .spawn(move || {
                        handle_ipc_connection(
                            stream,
                            ipc_guard,
                            mode,
                            owner_pid,
                            &conn_token,
                            conn_on_activity.as_ref(),
                            &conn_queue,
                            &conn_shutdown,
                            &conn_waiters,
                        );
                    });
                if let Err(err) = spawn_res {
                    platform::write_error_log_safely(&format!(
                        "Collector IPC connection thread spawn failed: {err}"
                    ));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }

    shutdown_flag.store(true, Ordering::Release);
    {
        let (lock, cvar) = &*queue;
        if let Ok(mut guard) = lock.lock() {
            guard.shutdown = true;
            cvar.notify_all();
        }
    }

    if clear_collector_state_if_owner(owner_pid, &token) {
        listener.cleanup_socket_file();
    }
}

fn run_stale_cleanup_with_env(env_ov: Option<&HashMap<String, Option<String>>>) {
    if let Some(ov) = env_ov {
        platform::with_env_overrides(ov, || {
            let _ = crate::ingest::cleanup_stale_temp_db_dirs(Duration::from_secs(600));
        });
    } else {
        let _ = crate::ingest::cleanup_stale_temp_db_dirs(Duration::from_secs(600));
    }
}

fn spawn_stale_cleanup_thread(env_ov: Option<HashMap<String, Option<String>>>) {
    let _ = thread::Builder::new()
        .name("agy-pulse-tmp-cleanup".to_string())
        .spawn(move || run_stale_cleanup_with_env(env_ov.as_ref()));
}

struct WorkerDoneGuard(Arc<(Mutex<bool>, Condvar)>);

impl Drop for WorkerDoneGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.0;
        if let Ok(mut done) = lock.lock() {
            *done = true;
            cvar.notify_all();
        }
    }
}

fn run_worker_loop(
    queue: Arc<(Mutex<CollectorQueueState>, Condvar)>,
    shutdown_flag: Arc<AtomicBool>,
    worker_done: Arc<(Mutex<bool>, Condvar)>,
) {
    let _done_guard = WorkerDoneGuard(worker_done);
    let mut did_stale_cleanup = false;

    loop {
        let maybe_job = {
            let (lock, cvar) = &*queue;
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            while guard.pending_order.is_empty()
                && !guard.shutdown
                && !shutdown_flag.load(Ordering::Acquire)
            {
                if !did_stale_cleanup {
                    let (new_guard, wait_res) = cvar
                        .wait_timeout(guard, Duration::from_millis(100))
                        .unwrap_or_else(|e| e.into_inner());
                    guard = new_guard;
                    if wait_res.timed_out()
                        && guard.pending_order.is_empty()
                        && !guard.shutdown
                        && !shutdown_flag.load(Ordering::Acquire)
                    {
                        let env_ov = guard.last_env_overrides.clone();
                        did_stale_cleanup = true;
                        drop(guard);
                        spawn_stale_cleanup_thread(env_ov);
                        guard = match lock.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                } else {
                    guard = match cvar.wait(guard) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
            }

            if (guard.shutdown || shutdown_flag.load(Ordering::Acquire))
                && guard.pending_order.is_empty()
            {
                return;
            }

            let Some(conv_key) = guard.pending_order.pop_front() else {
                continue;
            };
            let Some(job) = guard.pending_jobs.remove(&conv_key) else {
                continue;
            };
            guard.worker_busy = true;
            Some((conv_key, job))
        };

        let Some((conv_key, job)) = maybe_job else {
            continue;
        };
        let last_env = Some(job.req.env_overrides.clone());

        let soft_budget = Duration::from_millis(job.req.soft_budget_ms.clamp(100, 10_000));
        let deadline = Instant::now() + soft_budget;

        let enrichment = platform::with_env_overrides(&job.req.env_overrides, || {
            let res = crate::app::collect_statusline_enrichment(
                &job.req.stdin_text,
                &job.req.fallback_cwd,
                job.req.stdin_ms,
                deadline,
            );
            let now_ms = current_epoch_ms();
            write_snapshots_and_prune(&conv_key, &res, now_ms);
            res
        });

        // Clear `worker_busy` BEFORE waking waiters so an immediate sequential
        // statusline invocation never observes a stale `worker_busy == true`
        // and receives a spurious `Busy` fallback.
        let should_cleanup_now = {
            let (lock, _) = &*queue;
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            guard.worker_busy = false;
            guard.last_activity = Instant::now();
            if !did_stale_cleanup && guard.pending_order.is_empty() {
                did_stale_cleanup = true;
                true
            } else {
                false
            }
        };

        let response = CollectorWireResponse::Ready(Box::new(CollectorReadyPayload {
            enabled: enrichment.enabled,
            snapshot: enrichment.snapshot,
            config: enrichment.config,
            summary: enrichment.summary,
        }));
        for tx in job.waiters {
            let _ = tx.send(response.clone());
        }

        if should_cleanup_now {
            spawn_stale_cleanup_thread(last_env);
        }
    }
}

fn resolve_headless_idle_and_watch_dir() -> (Option<Duration>, Option<PathBuf>) {
    let data_dir = platform::env_var("AGY_PULSE_DATA_DIR")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let idle_sec = platform::env_var("AGY_PULSE_COLLECTOR_IDLE_SEC")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| {
            if data_dir.is_some() {
                Some(DEFAULT_TEST_DATA_DIR_IDLE_SEC)
            } else {
                Some(DEFAULT_PROD_IDLE_SEC)
            }
        });

    let idle_timeout = idle_sec.filter(|&s| s > 0).map(Duration::from_secs);
    (idle_timeout, data_dir)
}

/// Entrypoint for `agy-pulse --collector-bg` standalone headless collector daemon.
pub fn run_collector_bg_cli(_args: &[String]) -> i32 {
    platform::install_shutdown_signal_handler();
    // Detach cwd from any workspace or test TempDir so cleanup never holds directory handles.
    let _ = env::set_current_dir(env::temp_dir());

    let my_exe_mtime = platform::current_exe_mtime_ms();
    if let Some(existing) = read_collector_state()
        && platform::is_process_alive(existing.pid)
    {
        let same_binary = is_same_collector_binary(&existing, my_exe_mtime);
        if same_binary {
            let _ = release_collector_spawn_lock();
            return 0;
        }
        takeover_existing_collector(&existing);
    }

    let (idle_timeout, watch_data_dir) = resolve_headless_idle_and_watch_dir();
    let handle = match start_collector_internal(
        CollectorMode::Headless,
        idle_timeout,
        None,
        None,
        watch_data_dir,
    ) {
        Ok(h) => {
            let _ = release_collector_spawn_lock();
            h
        }
        Err(err) => {
            let _ = release_collector_spawn_lock();
            platform::write_error_log_safely(&format!("Collector daemon start error: {err}"));
            return 1;
        }
    };

    let mut handle = handle;
    if let Some(ipc) = handle.ipc_thread.take() {
        let _ = ipc.join();
    }
    handle.shutdown_flag.store(true, Ordering::Release);
    {
        let (lock, cvar) = &*handle.queue;
        if let Ok(mut guard) = lock.lock() {
            guard.shutdown = true;
            cvar.notify_all();
        }
    }
    let mut handle = handle;
    if let Some(ipc) = handle.ipc_thread.take() {
        let _ = ipc.join();
    }
    handle.shutdown();
    0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        CollectorQueueState, CollectorServerHandle, is_same_collector_binary,
    };
    use crate::collector::protocol::{
        COLLECTOR_APP_ID, CollectorRequest, CollectorStateFile, CollectorWireResponse,
    };

    fn sample_state(exe_mtime_ms: i64) -> CollectorStateFile {
        CollectorStateFile {
            app: COLLECTOR_APP_ID.to_string(),
            pid: std::process::id(),
            version: crate::app::VERSION.to_string(),
            exe_mtime_ms,
            mode: "collector".to_string(),
            socket_path: None,
            tcp_port: None,
            token: "tok".to_string(),
            started_at_ms: 1_700_000_000_000,
        }
    }

    fn sample_collect_req(conv_key: &str, stdin_text: &str) -> CollectorRequest {
        CollectorRequest {
            token: "tok".to_string(),
            stdin_text: stdin_text.to_string(),
            fallback_cwd: PathBuf::from("."),
            stdin_ms: 1,
            conversation_key: conv_key.to_string(),
            has_warm_snapshot: false,
            wait_ms: 100,
            soft_budget_ms: 500,
            env_overrides: HashMap::new(),
        }
    }

    #[test]
    fn server_and_client_agree_on_exe_mtime_zero_fallback() {
        assert!(is_same_collector_binary(&sample_state(0), 1_700_000_000_000));
        assert!(is_same_collector_binary(&sample_state(1_700_000_000_000), 0));
        assert!(is_same_collector_binary(
            &sample_state(1_700_000_000_123),
            1_700_000_000_123
        ));
        assert!(!is_same_collector_binary(
            &sample_state(1_700_000_000_123),
            1_700_000_000_999
        ));
    }

    #[test]
    fn queue_bounds_distinct_keys_while_preserving_debounce_merge() {
        let mut q = CollectorQueueState::with_capacity(2);
        let (tx_a1, _rx_a1) = mpsc::sync_channel::<CollectorWireResponse>(1);
        let (tx_b, _rx_b) = mpsc::sync_channel::<CollectorWireResponse>(1);
        let (tx_c, _rx_c) = mpsc::sync_channel::<CollectorWireResponse>(1);
        let (tx_a2, _rx_a2) = mpsc::sync_channel::<CollectorWireResponse>(1);

        assert!(q.can_accept("conv-a"));
        assert!(q.enqueue(sample_collect_req("conv-a", "a1"), Some(tx_a1)));
        assert!(q.enqueue(sample_collect_req("conv-b", "b1"), Some(tx_b)));

        assert!(!q.can_accept("conv-c"));
        assert!(!q.enqueue(sample_collect_req("conv-c", "c1"), Some(tx_c)));
        assert_eq!(q.pending_order.len(), 2);

        // Existing conversation key must still debounce-merge even when queue is at capacity.
        assert!(q.can_accept("conv-a"));
        assert!(q.enqueue(sample_collect_req("conv-a", "a2"), Some(tx_a2)));
        assert_eq!(q.pending_order.len(), 2);
        let Some(merged) = q.pending_jobs.get("conv-a") else {
            panic!("conv-a present");
        };
        assert_eq!(merged.req.stdin_text, "a2");
        assert_eq!(merged.waiters.len(), 2);
    }

    #[test]
    fn shutdown_uses_bounded_wait_when_worker_is_stalled() {
        let queue = Arc::new((
            Mutex::new(CollectorQueueState::with_capacity(2)),
            Condvar::new(),
        ));
        let worker_done = Arc::new((Mutex::new(false), Condvar::new()));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker_thread = thread::spawn(move || {
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });

        let mut handle = CollectorServerHandle {
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            queue,
            worker_done,
            ipc_thread: None,
            worker_thread: Some(worker_thread),
        };

        let start = Instant::now();
        handle.shutdown();
        let elapsed = start.elapsed();
        let _ = release_tx.send(());

        assert!(
            elapsed < Duration::from_millis(2400),
            "shutdown must not block indefinitely on stalled worker (took {elapsed:?})"
        );
    }
}
