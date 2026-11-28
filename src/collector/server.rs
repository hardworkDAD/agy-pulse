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
    read_collector_state, read_frame, release_collector_spawn_lock, send_shutdown_to_state,
    write_collector_state, write_frame,
};
use super::snapshot::write_snapshots_and_prune;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(15);
const STREAM_IO_TIMEOUT: Duration = Duration::from_millis(200);
const DATA_DIR_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const TAKEOVER_WAIT_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_CONCURRENT_WAITERS: usize = 16;
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
    pending_order: VecDeque<String>,
    pending_jobs: HashMap<String, PendingJob>,
    shutdown: bool,
    last_activity: Instant,
    last_env_overrides: Option<HashMap<String, Option<String>>>,
}

impl CollectorQueueState {
    fn enqueue(
        &mut self,
        req: CollectorRequest,
        waiter: Option<SyncSender<CollectorWireResponse>>,
    ) {
        let key = req.conversation_key.clone();
        self.last_env_overrides = Some(req.env_overrides.clone());
        if let Some(existing) = self.pending_jobs.get_mut(&key) {
            existing.req = req;
            if let Some(tx) = waiter {
                existing.waiters.push(tx);
            }
        } else {
            let mut waiters = Vec::new();
            if let Some(tx) = waiter {
                waiters.push(tx);
            }
            self.pending_order.push_back(key.clone());
            self.pending_jobs.insert(key, PendingJob { req, waiters });
        }
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
    ipc_thread: Option<JoinHandle<()>>,
    worker_thread: Option<JoinHandle<()>>,
}

impl CollectorServerHandle {
    /// Gracefully shut down any standalone headless collector and start the embedded collector
    /// sharing lifecycle and keepalive activity with the unified dashboard server.
    pub fn start_embedded(
        external_shutdown: Arc<AtomicBool>,
        on_activity: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        if let Some(existing) = read_collector_state()
            && existing.mode == CollectorMode::Headless.as_str()
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

    /// Signal the collector IPC and worker threads to stop and join them.
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
            let _ = handle.join();
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
    let _ = send_shutdown_to_state(existing, Duration::from_millis(200));
    let deadline = Instant::now() + TAKEOVER_WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if !platform::is_process_alive(existing.pid) {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
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
        version: crate::VERSION.to_string(),
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
        Mutex::new(CollectorQueueState {
            worker_busy: false,
            pending_order: VecDeque::new(),
            pending_jobs: HashMap::new(),
            shutdown: false,
            last_activity: Instant::now(),
            last_env_overrides: None,
        }),
        Condvar::new(),
    ));

    let worker_queue = Arc::clone(&queue);
    let worker_shutdown = Arc::clone(&shutdown_flag);
    let worker_thread = thread::Builder::new()
        .name("agy-pulse-collector-worker".to_string())
        .spawn(move || run_worker_loop(worker_queue, worker_shutdown))?;

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
        ipc_thread: Some(ipc_thread),
        worker_thread: Some(worker_thread),
    })
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
                let _ = stream.set_read_timeout(Some(STREAM_IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(STREAM_IO_TIMEOUT));
                let req: CollectorWireRequest = match read_frame(&mut stream) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if req.token() != token {
                    let _ = write_frame(
                        &mut stream,
                        &CollectorWireResponse::Error("unauthorized".to_string()),
                    );
                    continue;
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
                                version: crate::VERSION.to_string(),
                                mode: mode.as_str().to_string(),
                                worker_busy: busy,
                            },
                        );
                    }
                    CollectorWireRequest::Shutdown { .. } => {
                        shutdown_flag.store(true, Ordering::Release);
                        let (lock, cvar) = &*queue;
                        if let Ok(mut guard) = lock.lock() {
                            guard.shutdown = true;
                            cvar.notify_all();
                        }
                        let _ = write_frame(&mut stream, &CollectorWireResponse::Ack);
                        break;
                    }
                    CollectorWireRequest::Collect(collect_req) => {
                        if let Some(ref cb) = on_activity {
                            cb();
                        }
                        let wait_ms = collect_req.wait_ms;
                        enum EnqueueOutcome {
                            ReplyBusy,
                            ReplyError(&'static str),
                            Wait(mpsc::Receiver<CollectorWireResponse>),
                        }
                        let outcome = {
                            let (lock, cvar) = &*queue;
                            match lock.lock() {
                                Err(_) => EnqueueOutcome::ReplyError("queue poisoned"),
                                Ok(mut guard) => {
                                    guard.last_activity = Instant::now();
                                    if guard.shutdown {
                                        EnqueueOutcome::ReplyError("shutting down")
                                    } else if (guard.worker_busy && collect_req.has_warm_snapshot)
                                        || wait_ms == 0
                                    {
                                        guard.enqueue(collect_req, None);
                                        cvar.notify_one();
                                        EnqueueOutcome::ReplyBusy
                                    } else {
                                        let prev = active_waiters.fetch_add(1, Ordering::AcqRel);
                                        if prev >= MAX_CONCURRENT_WAITERS {
                                            active_waiters.fetch_sub(1, Ordering::AcqRel);
                                            guard.enqueue(collect_req, None);
                                            cvar.notify_one();
                                            EnqueueOutcome::ReplyBusy
                                        } else {
                                            let (tx, rx) =
                                                mpsc::sync_channel::<CollectorWireResponse>(1);
                                            guard.enqueue(collect_req, Some(tx));
                                            cvar.notify_one();
                                            EnqueueOutcome::Wait(rx)
                                        }
                                    }
                                }
                            }
                        };

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
                            EnqueueOutcome::Wait(rx) => {
                                let waiters_counter = Arc::clone(&active_waiters);
                                let spawn_res = thread::Builder::new()
                                    .name("agy-pulse-collector-waiter".to_string())
                                    .spawn(move || {
                                        struct WaiterGuard(Arc<AtomicUsize>);
                                        impl Drop for WaiterGuard {
                                            fn drop(&mut self) {
                                                self.0.fetch_sub(1, Ordering::AcqRel);
                                            }
                                        }
                                        let _guard = WaiterGuard(waiters_counter);
                                        let resp = match rx.recv_timeout(Duration::from_millis(wait_ms)) {
                                            Ok(r) => r,
                                            Err(_) => CollectorWireResponse::Busy,
                                        };
                                        let _ = write_frame(&mut stream, &resp);
                                    });
                                if spawn_res.is_err() {
                                    active_waiters.fetch_sub(1, Ordering::AcqRel);
                                }
                            }
                        }
                    }
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

fn run_worker_loop(
    queue: Arc<(Mutex<CollectorQueueState>, Condvar)>,
    shutdown_flag: Arc<AtomicBool>,
) {
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
            let res = crate::collect_statusline_enrichment(
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
        let same_binary =
            existing.version == crate::VERSION && existing.exe_mtime_ms == my_exe_mtime;
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
    if let Some(worker) = handle.worker_thread.take() {
        let _ = worker.join();
    }
    0
}
