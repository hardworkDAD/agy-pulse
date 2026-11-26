use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::{
    QuotaSnapshot, SyncConvRecord, SyncStepRecord, UNASSIGNED_ACCOUNT_ID, normalize_account_id,
};
use crate::ledger::LedgerStore;
use crate::platform;

use super::SyncError;
use super::SyncRemote;
use super::config::SyncConfig;
use super::protocol::{
    self, ChunkMeta, ChunkRecord, DeviceManifest, MANIFEST_CHUNK_STATE_KEY,
    ManifestQuotaSnapshot, SYNC_WIRE_FORMAT_VERSION,
};

const SYNC_LOCK_FILENAME: &str = "agy-pulse-sync.lock";
const SYNC_LOCK_MAX_TTL_MS: u64 = 10 * 60 * 1000;
const SYNC_LOCK_FALLBACK_TTL_MS: u64 = 30_000;

static SYNC_LOCK_SEQ: AtomicU64 = AtomicU64::new(1);

/// Execution status classification for `push`, `pull`, and `sync_both`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncOutcomeKind {
    #[default]
    Completed,
    DryRun,
    AlreadyRunning,
}

/// Structured summary returned by `push`, `pull`, and `sync_both`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub kind: SyncOutcomeKind,
    pub locked: bool,
    pub dry_run: bool,
    pub accounts_processed: usize,
    pub chunks_uploaded: usize,
    pub chunks_downloaded: usize,
    pub chunks_skipped: usize,
    pub manifests_uploaded: usize,
    pub devices_synced: usize,
    pub steps_imported: usize,
    pub remote_cache_cleared: bool,
    pub duration_ms: u64,
}

impl SyncOutcome {
    fn already_running(dry_run: bool, duration_ms: u64) -> Self {
        Self {
            kind: SyncOutcomeKind::AlreadyRunning,
            locked: true,
            dry_run,
            duration_ms,
            ..Default::default()
        }
    }
}

/// Summary returned by `prune_device`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneOutcome {
    pub target_device_id: String,
    pub is_local: bool,
    pub dry_run: bool,
    pub remote_objects_deleted: usize,
    pub local_rows_deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyncLockPayload {
    pid: u32,
    token: String,
    ts: i64,
}

struct SyncLockGuard {
    lock_path: PathBuf,
    token: String,
}

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        let Ok(raw) = fs::read_to_string(&self.lock_path) else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<SyncLockPayload>(&raw) else {
            return;
        };
        if parsed.token == self.token {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sync_lock_path_for_ledger(ledger_path: &Path) -> PathBuf {
    let dir = ledger_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join(SYNC_LOCK_FILENAME)
}

fn acquire_sync_lock(ledger_path: &Path, now_ms: i64) -> Result<Option<SyncLockGuard>, SyncError> {
    let lock_path = sync_lock_path_for_ledger(ledger_path);
    let seq = SYNC_LOCK_SEQ.fetch_add(1, Ordering::Relaxed);
    let token = format!("{}-{now_ms}-{seq}", std::process::id());
    let payload = SyncLockPayload {
        pid: std::process::id(),
        token: token.clone(),
        ts: now_ms,
    };
    let bytes = serde_json::to_vec(&payload)?;

    for _attempt in 0..2 {
        match platform::create_exclusive_file_0600(&lock_path, &bytes)? {
            true => return Ok(Some(SyncLockGuard { lock_path, token })),
            false => {
                let meta = match fs::metadata(&lock_path) {
                    Ok(m) => m,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(SyncError::Io(err)),
                };
                let age_ms = meta
                    .modified()
                    .ok()
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let is_active = match fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<SyncLockPayload>(&raw).ok())
                {
                    Some(parsed) if parsed.pid > 0 => {
                        platform::is_process_alive(parsed.pid) && age_ms < SYNC_LOCK_MAX_TTL_MS
                    }
                    _ => age_ms < SYNC_LOCK_FALLBACK_TTL_MS,
                };

                if is_active {
                    return Ok(None);
                }

                match fs::remove_file(&lock_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(SyncError::Io(err)),
                }
            }
        }
    }

    Ok(None)
}

fn resolve_effective_device_name(cfg: &SyncConfig, local_device_id: &str) -> String {
    cfg.device_name
        .as_deref()
        .and_then(platform::sanitize_device_name)
        .or_else(platform::current_hostname)
        .unwrap_or_else(|| local_device_id.to_string())
}

fn ensure_remote_fingerprint(
    store: &LedgerStore,
    cfg: &SyncConfig,
    dry_run: bool,
) -> Result<bool, SyncError> {
    let current_fp = cfg.remote_fingerprint();
    let meta = store.get_sync_meta()?;
    match meta.last_sync_remote_fingerprint {
        None => {
            if !dry_run {
                store.set_sync_meta_field("last_sync_remote_fingerprint", Some(&current_fp))?;
            }
            Ok(false)
        }
        Some(ref prev_fp) if prev_fp != &current_fp => {
            if !dry_run {
                store.clear_remote_data()?;
                store.clear_local_push_chunk_states()?;
                store.set_sync_meta_field("last_sync_remote_fingerprint", Some(&current_fp))?;
            }
            Ok(true)
        }
        Some(_) => Ok(false),
    }
}

fn record_sync_error_best_effort(store: &LedgerStore, err: &SyncError) {
    let sanitized = format!("{err}")
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect::<String>();
    let _ = store.set_sync_meta_field("last_sync_error", Some(&sanitized));
}

fn push_inner(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    dry_run: bool,
    now_ms: i64,
    outcome: &mut SyncOutcome,
) -> Result<(), SyncError> {
    let local_device_id = store.local_device_id()?;
    let device_name = resolve_effective_device_name(cfg, &local_device_id);
    let prefix = protocol::normalize_prefix(&cfg.prefix);
    let accounts = store.list_local_sync_accounts()?;

    for account_id in &accounts {
        outcome.accounts_processed += 1;
        let monthly = store.export_local_account_monthly_chunks(account_id)?;
        let plan_tier = store.read_local_device_plan_tier(account_id)?;
        let quota_snapshot = store
            .read_local_account_quota_snapshot(account_id)?
            .map(|q| ManifestQuotaSnapshot {
                observed_at_ms: q.observed_at_ms,
                quota: q.quota,
            });

        let mut chunks_meta: BTreeMap<String, ChunkMeta> = BTreeMap::new();
        let mut any_chunk_uploaded = false;

        for (month_key, export) in &monthly {
            let mut records = Vec::with_capacity(export.convs.len() + export.steps.len());
            for c in &export.convs {
                records.push(ChunkRecord::Conv {
                    conversation_id: c.conversation_id.clone(),
                    created_at_ms: c.created_at_ms,
                    cascade_id: c.cascade_id.clone(),
                    parent_conversation_id: c.parent_conversation_id.clone(),
                    root_conversation_id: c.root_conversation_id.clone(),
                    source: c.source.clone(),
                });
            }
            for s in &export.steps {
                records.push(ChunkRecord::Step {
                    step_uid: s.step_uid.clone(),
                    conversation_id: s.conversation_id.clone(),
                    gen_idx: s.gen_idx,
                    entry_idx: s.entry_idx,
                    step_index: s.step_index,
                    ts_ms: s.ts_ms,
                    model: s.model.clone(),
                    raw_model: s.raw_model.clone(),
                    input_tokens: s.input_tokens,
                    cache_read_tokens: s.cache_read_tokens,
                    cache_write_tokens: s.cache_write_tokens,
                    output_tokens: s.output_tokens,
                    cost_usd: s.cost_usd,
                    stream_sec: s.stream_sec,
                    ttft_sec: s.ttft_sec,
                    valid_stream: s.valid_stream,
                    valid_ttft: s.valid_ttft,
                    parent_conversation_id: s.parent_conversation_id.clone(),
                    root_conversation_id: s.root_conversation_id.clone(),
                    source: s.source.clone(),
                });
            }

            let (gz_bytes, sha256_hex) = protocol::encode_chunk_gz(&records)?;
            let step_count = export.steps.len() as u64;
            let bytes = gz_bytes.len() as u64;

            let prev_hash =
                store.get_sync_chunk_hash(account_id, &local_device_id, month_key, "push")?;
            if prev_hash.as_deref() == Some(sha256_hex.as_str()) {
                outcome.chunks_skipped += 1;
            } else {
                outcome.chunks_uploaded += 1;
                any_chunk_uploaded = true;
                if !dry_run {
                    let ckey =
                        protocol::chunk_key(&prefix, account_id, &local_device_id, month_key)?;
                    remote.put(&ckey, &gz_bytes, "application/gzip")?;
                    store.set_sync_chunk_hash(
                        account_id,
                        &local_device_id,
                        month_key,
                        "push",
                        &sha256_hex,
                        now_ms,
                    )?;
                }
            }

            chunks_meta.insert(
                month_key.clone(),
                ChunkMeta {
                    sha256: sha256_hex,
                    step_count,
                    bytes,
                },
            );
        }

        let manifest = DeviceManifest {
            format_version: SYNC_WIRE_FORMAT_VERSION,
            account_id: account_id.clone(),
            device_id: local_device_id.clone(),
            device_name: device_name.clone(),
            plan_tier,
            updated_at_ms: now_ms,
            quota_snapshot,
            chunks: chunks_meta,
        };

        let semantic_hash = protocol::manifest_semantic_sha256(&manifest)?;
        let prev_manifest_hash = store.get_sync_chunk_hash(
            account_id,
            &local_device_id,
            MANIFEST_CHUNK_STATE_KEY,
            "push",
        )?;

        if any_chunk_uploaded || prev_manifest_hash.as_deref() != Some(semantic_hash.as_str()) {
            outcome.manifests_uploaded += 1;
            if !dry_run {
                let mkey = protocol::manifest_key(&prefix, account_id, &local_device_id)?;
                let mbytes = serde_json::to_vec_pretty(&manifest)?;
                remote.put(&mkey, &mbytes, "application/json")?;
                store.set_sync_chunk_hash(
                    account_id,
                    &local_device_id,
                    MANIFEST_CHUNK_STATE_KEY,
                    "push",
                    &semantic_hash,
                    now_ms,
                )?;
            }
        }
    }

    if !dry_run {
        store.set_sync_meta_field("last_sync_push_ms", Some(&now_ms.to_string()))?;
        store.set_sync_meta_field("last_sync_error", None)?;
    }

    Ok(())
}

fn pull_inner(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    dry_run: bool,
    now_ms: i64,
    outcome: &mut SyncOutcome,
) -> Result<(), SyncError> {
    let local_device_id = store.local_device_id()?;
    let prefix = protocol::normalize_prefix(&cfg.prefix);
    let accounts_pfx = protocol::accounts_prefix(&prefix)?;

    let mut keys = remote.list(&accounts_pfx)?;
    keys.sort();

    let mut accounts_seen = std::collections::BTreeSet::new();

    for key in keys {
        let Some(parts) = protocol::parse_manifest_key(&prefix, &key) else {
            continue;
        };
        if parts.device_id == local_device_id {
            continue;
        }

        let Some(manifest_bytes) = remote.get(&key)? else {
            continue;
        };
        let manifest: DeviceManifest = serde_json::from_slice(&manifest_bytes)?;
        if manifest.format_version != SYNC_WIRE_FORMAT_VERSION {
            return Err(SyncError::Protocol(format!(
                "unsupported manifest format_version {} at {key}",
                manifest.format_version
            )));
        }

        let manifest_account = normalize_account_id(&manifest.account_id)
            .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
        let manifest_account_key = protocol::encode_account_key(&manifest_account)?;
        if manifest_account_key != parts.account_key || manifest_account != parts.account_id {
            return Err(SyncError::Protocol(format!(
                "manifest account_id '{}' does not match path account '{}'",
                manifest.account_id, parts.account_id
            )));
        }
        if manifest.device_id.trim() != parts.device_id {
            return Err(SyncError::Protocol(format!(
                "manifest device_id '{}' does not match path device '{}'",
                manifest.device_id, parts.device_id
            )));
        }
        if manifest.device_id.trim() == local_device_id {
            return Err(SyncError::Protocol(
                "manifest device_id collides with local device_id".to_string(),
            ));
        }

        accounts_seen.insert(parts.account_id.clone());
        outcome.devices_synced += 1;

        let effective_device_name = platform::sanitize_device_name(&manifest.device_name)
            .unwrap_or_else(|| parts.device_id.clone());

        if !dry_run {
            store.upsert_sync_device(
                &parts.account_id,
                &parts.device_id,
                &effective_device_name,
                false,
                manifest.updated_at_ms.max(0),
                manifest.plan_tier.as_deref(),
            )?;

            if let Some(ref q) = manifest.quota_snapshot {
                store.upsert_quota_snapshot(&QuotaSnapshot {
                    account_id: parts.account_id.clone(),
                    device_id: parts.device_id.clone(),
                    device_name: effective_device_name.clone(),
                    plan_tier: manifest.plan_tier.clone(),
                    observed_at_ms: q.observed_at_ms,
                    quota: q.quota.clone(),
                })?;
            }
        }

        for (month_key, chunk_meta) in &manifest.chunks {
            protocol::validate_single_segment(month_key, "month_key")?;
            let prev_hash = store.get_sync_chunk_hash(
                &parts.account_id,
                &parts.device_id,
                month_key,
                "pull",
            )?;
            if prev_hash.as_deref() == Some(chunk_meta.sha256.as_str()) {
                outcome.chunks_skipped += 1;
                continue;
            }

            outcome.chunks_downloaded += 1;
            if dry_run {
                continue;
            }

            let ckey =
                protocol::chunk_key(&prefix, &parts.account_id, &parts.device_id, month_key)?;
            let Some(gz_bytes) = remote.get(&ckey)? else {
                return Err(SyncError::Protocol(format!(
                    "missing remote chunk referenced by manifest: {ckey}"
                )));
            };
            let actual_sha = protocol::sha256_hex(&gz_bytes);
            if actual_sha != chunk_meta.sha256 {
                return Err(SyncError::Protocol(format!(
                    "sha256 mismatch for remote chunk {ckey}: expected {}, got {actual_sha}",
                    chunk_meta.sha256
                )));
            }

            let records = protocol::decode_chunk_gz(&gz_bytes)?;
            let mut convs = Vec::new();
            let mut steps = Vec::new();
            for rec in records {
                match rec {
                    ChunkRecord::Conv {
                        conversation_id,
                        created_at_ms,
                        cascade_id,
                        parent_conversation_id,
                        root_conversation_id,
                        source,
                    } => convs.push(SyncConvRecord {
                        conversation_id,
                        created_at_ms,
                        cascade_id,
                        parent_conversation_id,
                        root_conversation_id,
                        source,
                    }),
                    ChunkRecord::Step {
                        step_uid,
                        conversation_id,
                        gen_idx,
                        entry_idx,
                        step_index,
                        ts_ms,
                        model,
                        raw_model,
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        output_tokens,
                        cost_usd,
                        stream_sec,
                        ttft_sec,
                        valid_stream,
                        valid_ttft,
                        parent_conversation_id,
                        root_conversation_id,
                        source,
                    } => steps.push(SyncStepRecord {
                        step_uid,
                        conversation_id,
                        gen_idx,
                        entry_idx,
                        step_index,
                        ts_ms,
                        model,
                        raw_model,
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        output_tokens,
                        cost_usd,
                        stream_sec,
                        ttft_sec,
                        valid_stream,
                        valid_ttft,
                        parent_conversation_id,
                        root_conversation_id,
                        source,
                    }),
                }
            }

            let imported = store.import_remote_chunk(
                &parts.account_id,
                &parts.device_id,
                month_key,
                &actual_sha,
                &convs,
                &steps,
                now_ms,
            )?;
            outcome.steps_imported += imported;
        }
    }

    outcome.accounts_processed += accounts_seen.len();

    if !dry_run {
        store.set_sync_meta_field("last_sync_pull_ms", Some(&now_ms.to_string()))?;
        store.set_sync_meta_field("last_sync_error", None)?;
    }

    Ok(())
}

/// Push local ledger monthly chunks and manifests to `remote` under `agy-pulse-sync.lock`.
pub fn push(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    dry_run: bool,
) -> Result<SyncOutcome, SyncError> {
    let started = Instant::now();
    let now_ms = current_epoch_ms();
    let Some(_guard) = acquire_sync_lock(store.ledger_path(), now_ms)? else {
        return Ok(SyncOutcome::already_running(
            dry_run,
            started.elapsed().as_millis() as u64,
        ));
    };

    let mut outcome = SyncOutcome {
        kind: if dry_run {
            SyncOutcomeKind::DryRun
        } else {
            SyncOutcomeKind::Completed
        },
        dry_run,
        ..Default::default()
    };

    let res = (|| -> Result<(), SyncError> {
        outcome.remote_cache_cleared = ensure_remote_fingerprint(store, cfg, dry_run)?;
        push_inner(store, remote, cfg, dry_run, now_ms, &mut outcome)
    })();

    if let Err(err) = res {
        if !dry_run {
            record_sync_error_best_effort(store, &err);
        }
        return Err(err);
    }

    outcome.duration_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// Pull remote device manifests and monthly chunks into `store` under `agy-pulse-sync.lock`.
pub fn pull(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    dry_run: bool,
) -> Result<SyncOutcome, SyncError> {
    let started = Instant::now();
    let now_ms = current_epoch_ms();
    let Some(_guard) = acquire_sync_lock(store.ledger_path(), now_ms)? else {
        return Ok(SyncOutcome::already_running(
            dry_run,
            started.elapsed().as_millis() as u64,
        ));
    };

    let mut outcome = SyncOutcome {
        kind: if dry_run {
            SyncOutcomeKind::DryRun
        } else {
            SyncOutcomeKind::Completed
        },
        dry_run,
        ..Default::default()
    };

    let res = (|| -> Result<(), SyncError> {
        outcome.remote_cache_cleared = ensure_remote_fingerprint(store, cfg, dry_run)?;
        pull_inner(store, remote, cfg, dry_run, now_ms, &mut outcome)
    })();

    if let Err(err) = res {
        if !dry_run {
            record_sync_error_best_effort(store, &err);
        }
        return Err(err);
    }

    outcome.duration_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// Execute `pull` followed by `push` under a single `agy-pulse-sync.lock` acquisition.
pub fn sync_both(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    dry_run: bool,
) -> Result<SyncOutcome, SyncError> {
    let started = Instant::now();
    let now_ms = current_epoch_ms();
    let Some(_guard) = acquire_sync_lock(store.ledger_path(), now_ms)? else {
        return Ok(SyncOutcome::already_running(
            dry_run,
            started.elapsed().as_millis() as u64,
        ));
    };

    let mut outcome = SyncOutcome {
        kind: if dry_run {
            SyncOutcomeKind::DryRun
        } else {
            SyncOutcomeKind::Completed
        },
        dry_run,
        ..Default::default()
    };

    let res = (|| -> Result<(), SyncError> {
        outcome.remote_cache_cleared = ensure_remote_fingerprint(store, cfg, dry_run)?;
        pull_inner(store, remote, cfg, dry_run, now_ms, &mut outcome)?;
        push_inner(store, remote, cfg, dry_run, now_ms, &mut outcome)
    })();

    if let Err(err) = res {
        if !dry_run {
            record_sync_error_best_effort(store, &err);
        }
        return Err(err);
    }

    outcome.duration_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// Remove all remote objects under `devices/<target_device_id>/` across all accounts,
/// and clean up local SQLite state (deleting remote cache rows for remote devices, or clearing
/// only local `push` chunk state when `force == true` on the local device).
pub fn prune_device(
    store: &LedgerStore,
    remote: &dyn SyncRemote,
    cfg: &SyncConfig,
    target_device_id: &str,
    force: bool,
    dry_run: bool,
) -> Result<PruneOutcome, SyncError> {
    let trimmed_target = protocol::validate_single_segment(target_device_id.trim(), "device_id")?;
    let now_ms = current_epoch_ms();
    let Some(_guard) = acquire_sync_lock(store.ledger_path(), now_ms)? else {
        return Err(SyncError::AlreadyRunning);
    };

    let local_device_id = store.local_device_id()?;
    let is_local = trimmed_target == local_device_id;
    if is_local && !force {
        return Err(SyncError::Config(format!(
            "refusing to prune local device '{trimmed_target}' without --force"
        )));
    }

    let prefix = protocol::normalize_prefix(&cfg.prefix);
    let accounts_pfx = protocol::accounts_prefix(&prefix)?;
    let mut keys = remote.list(&accounts_pfx)?;
    keys.sort();

    let matching: Vec<String> = keys
        .into_iter()
        .filter(|k| protocol::key_belongs_to_device(&prefix, k, trimmed_target))
        .collect();

    let remote_objects_deleted = matching.len();
    let mut local_rows_deleted = 0usize;

    if !dry_run {
        for key in &matching {
            remote.delete(key)?;
        }
        if is_local {
            local_rows_deleted = store.clear_local_push_chunk_states()?;
        } else {
            local_rows_deleted = store.delete_device_data(trimmed_target)?;
        }
    }

    Ok(PruneOutcome {
        target_device_id: trimmed_target.to_string(),
        is_local,
        dry_run,
        remote_objects_deleted,
        local_rows_deleted,
    })
}
