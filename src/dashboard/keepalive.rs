use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEFAULT_CLOSE_GRACE_MS: u64 = 15_000;
pub const DEFAULT_PING_LEASE_MS: u64 = 150_000;
pub const DEFAULT_SSE_PING_INTERVAL_MS: u64 = 5_000;

/// Resolve the grace window after the last dashboard page disconnects before auto-shutdown.
pub fn resolve_close_grace_ms(override_ms: Option<u64>) -> u64 {
    if let Some(ms) = override_ms {
        return ms;
    }
    env::var("AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CLOSE_GRACE_MS)
}

/// Resolve the lease TTL for unconditional `POST /api/ping?client=<id>` heartbeats.
pub fn resolve_ping_lease_ms() -> u64 {
    env::var("AGY_PULSE_DASHBOARD_PING_LEASE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_PING_LEASE_MS)
}

/// Resolve the server-sent `: ping\n\n` frame interval for active SSE streams.
pub fn resolve_sse_ping_interval_ms() -> u64 {
    env::var("AGY_PULSE_DASHBOARD_SSE_PING_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_SSE_PING_INTERVAL_MS)
}

struct KeepaliveInner {
    ever_had_client: bool,
    active_sse: HashMap<u64, String>,
    leases: HashMap<String, Instant>,
    last_activity: Instant,
    zero_clients_since: Option<Instant>,
    next_conn_id: u64,
}

impl KeepaliveInner {
    fn prune_expired_leases(&mut self, now: Instant) {
        self.leases.retain(|_, expires_at| *expires_at > now);
    }

    fn distinct_active_clients(&self) -> usize {
        if self.active_sse.is_empty() && self.leases.is_empty() {
            return 0;
        }
        let mut seen: HashSet<&str> =
            HashSet::with_capacity(self.active_sse.len() + self.leases.len());
        for cid in self.active_sse.values() {
            seen.insert(cid.as_str());
        }
        for cid in self.leases.keys() {
            seen.insert(cid.as_str());
        }
        seen.len()
    }

    fn sync_zero_timer(&mut self, now: Instant) {
        if self.distinct_active_clients() > 0 {
            self.zero_clients_since = None;
        } else if self.ever_had_client && self.zero_clients_since.is_none() {
            self.zero_clients_since = Some(now);
        }
    }
}

/// Coordinates active page SSE streams, unconditional ping leases, and close-grace auto-exit.
pub(super) struct KeepaliveState {
    idle_timeout_ms: u64,
    close_grace: Duration,
    lease_ttl: Duration,
    inner: Mutex<KeepaliveInner>,
}

impl KeepaliveState {
    pub(super) fn new(
        idle_timeout_ms: u64,
        close_grace_ms: u64,
        lease_ttl_ms: u64,
        now: Instant,
    ) -> Self {
        Self {
            idle_timeout_ms,
            close_grace: Duration::from_millis(close_grace_ms),
            lease_ttl: Duration::from_millis(lease_ttl_ms.max(1)),
            inner: Mutex::new(KeepaliveInner {
                ever_had_client: false,
                active_sse: HashMap::new(),
                leases: HashMap::new(),
                last_activity: now,
                zero_clients_since: None,
                next_conn_id: 1,
            }),
        }
    }

    pub(super) fn close_grace_ms(&self) -> u64 {
        self.close_grace.as_millis() as u64
    }

    pub(super) fn register_sse(&self, client_id: Option<&str>) -> u64 {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        guard.prune_expired_leases(now);
        let conn_id = guard.next_conn_id;
        guard.next_conn_id = guard.next_conn_id.saturating_add(1);
        let key = client_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("_sse_{conn_id}"));
        guard.active_sse.insert(conn_id, key);
        guard.ever_had_client = true;
        guard.last_activity = now;
        guard.zero_clients_since = None;
        conn_id
    }

    pub(super) fn unregister_sse(&self, conn_id: u64) {
        if conn_id == 0 {
            return;
        }
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.prune_expired_leases(now);
        guard.active_sse.remove(&conn_id);
        guard.sync_zero_timer(now);
    }

    pub(super) fn is_sse_active(&self, conn_id: u64) -> bool {
        let Ok(guard) = self.inner.lock() else {
            return false;
        };
        guard.active_sse.contains_key(&conn_id)
    }

    pub(super) fn record_ping(&self, client_id: Option<&str>) -> usize {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        guard.prune_expired_leases(now);
        guard.last_activity = now;
        if let Some(cid) = client_id.map(str::trim).filter(|s| !s.is_empty()) {
            guard.ever_had_client = true;
            guard.leases.insert(cid.to_string(), now + self.lease_ttl);
            guard.zero_clients_since = None;
        }
        guard.distinct_active_clients()
    }

    pub(super) fn record_disconnect(&self, client_id: Option<&str>) -> usize {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        guard.prune_expired_leases(now);
        if let Some(cid) = client_id.map(str::trim).filter(|s| !s.is_empty()) {
            guard.leases.remove(cid);
            guard.active_sse.retain(|_, active_cid| active_cid != cid);
        }
        guard.sync_zero_timer(now);
        guard.distinct_active_clients()
    }

    pub(super) fn touch_page_load(&self) {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.prune_expired_leases(now);
        guard.last_activity = now;
        // Extend an active close grace window when a browser starts loading `/` (e.g., F5 reload).
        if guard.zero_clients_since.is_some() {
            guard.zero_clients_since = Some(now);
        }
    }

    pub(super) fn touch_activity(&self) {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.last_activity = now;
    }

    pub(super) fn active_client_count(&self) -> usize {
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        guard.prune_expired_leases(now);
        guard.sync_zero_timer(now);
        guard.distinct_active_clients()
    }

    pub(super) fn should_shutdown(&self) -> bool {
        if self.idle_timeout_ms == 0 {
            return false;
        }
        let now = Instant::now();
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        guard.prune_expired_leases(now);
        guard.sync_zero_timer(now);

        if guard.distinct_active_clients() > 0 {
            return false;
        }
        if guard.ever_had_client {
            match guard.zero_clients_since {
                Some(since) => now.duration_since(since) >= self.close_grace,
                None => false,
            }
        } else {
            now.duration_since(guard.last_activity) >= Duration::from_millis(self.idle_timeout_ms)
        }
    }
}
