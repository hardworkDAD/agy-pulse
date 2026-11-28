use std::net::TcpStream;
use std::time::Instant;

use serde::{Deserialize, Deserializer, Serialize};

use crate::config::{
    self, ALLOWED_THEME_COLORS, CacheHitThresholdsConfig, DisplayConfig, IconsConfig,
    LanguageSetting, PulseConfig, PulseEnvOverrides, QuotaStyle, THEME_PRESETS, ThemeConfig,
    ThresholdsConfig,
};
use crate::domain::UNASSIGNED_ACCOUNT_ID;
use crate::ledger::{redact_sync_error_message, validate_and_normalize_account_email};
use crate::platform;
use crate::render::resolve_language_from_locale;
use crate::sync::{
    self, DEFAULT_PUSH_INTERVAL_SEC, DEFAULT_SYNC_PREFIX, RemoteConfig, SyncConfig,
    SyncEnvOverrides, normalize_prefix, sanitize_url_for_display, validate_remote_url,
    validate_single_segment,
};

use super::http_wire::{
    HttpRequest, has_valid_csrf_token, is_json_content_type, is_strict_same_origin,
};
use super::server::{ServerSharedState, refresh_sync_identity_hints, send_json_value};

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn normalize_optional_text_preserving_control(raw: Option<String>) -> Option<String> {
    raw.and_then(|s| {
        if s.chars().any(char::is_control) {
            return Some(s);
        }
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_optional_text_for_dto(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Serialize)]
struct ConfigErrorPayload<'a> {
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionConflictPayload {
    error: &'static str,
    current_version: String,
}

fn send_config_error(stream: &mut TcpStream, status: u16, error: &str, field: Option<&str>) {
    send_json_value(stream, status, &ConfigErrorPayload { error, field }, &[]);
}

/// Enforce CSRF token, strict same-origin, and optional JSON Content-Type for `/api/config/*`.
pub(super) fn check_config_security(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
    require_json_body: bool,
) -> bool {
    if !has_valid_csrf_token(req, &shared.csrf_token) {
        send_config_error(stream, 403, "invalid or missing CSRF token", None);
        return false;
    }

    if !is_strict_same_origin(req, shared.port) {
        send_config_error(stream, 403, "cross-origin request rejected", None);
        return false;
    }

    if require_json_body && !is_json_content_type(req) {
        send_config_error(stream, 400, "Content-Type must be application/json", None);
        return false;
    }

    shared.keepalive.touch_activity();
    true
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PulseIconsDto {
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PulseDisplayDto {
    pub show_context_monitor: bool,
    pub show_model_cache: bool,
    pub show_current_dir: bool,
    pub show_cycle_mode: bool,
    pub show_speed: bool,
    pub show_breadcrumbs: bool,
    pub show_git_branch: bool,
    pub show_username: bool,
    pub show_daily_cost: bool,
    pub show_weekly_cost: bool,
    pub show_routed_cost: bool,
    pub show_dashboard_url: bool,
    pub dashboard_port: Option<u16>,
    pub dashboard_idle_timeout_sec: Option<u64>,
    pub breadcrumb_count: Option<u32>,
    pub use_nerd_fonts: bool,
    pub unicode: Option<bool>,
    pub quota_style: QuotaStyle,
    pub terminal_width: Option<u16>,
    pub username: Option<String>,
    pub cache_hit_thresholds: Option<CacheHitThresholdsConfig>,
    pub model_speed_expire_turns: Option<u64>,
    pub model_speed_expire_minutes: Option<f64>,
}

impl From<&DisplayConfig> for PulseDisplayDto {
    fn from(d: &DisplayConfig) -> Self {
        let def = DisplayConfig::builtin_defaults();
        Self {
            show_context_monitor: d
                .show_context_monitor
                .or(def.show_context_monitor)
                .unwrap_or(true),
            show_model_cache: d.show_model_cache.or(def.show_model_cache).unwrap_or(true),
            show_current_dir: d.show_current_dir.or(def.show_current_dir).unwrap_or(true),
            show_cycle_mode: d.show_cycle_mode.or(def.show_cycle_mode).unwrap_or(true),
            show_speed: d.show_speed.or(def.show_speed).unwrap_or(true),
            show_breadcrumbs: d.show_breadcrumbs.or(def.show_breadcrumbs).unwrap_or(false),
            show_git_branch: d.show_git_branch.or(def.show_git_branch).unwrap_or(false),
            show_username: d.show_username.or(def.show_username).unwrap_or(false),
            show_daily_cost: d.show_daily_cost.or(def.show_daily_cost).unwrap_or(false),
            show_weekly_cost: d.show_weekly_cost.or(def.show_weekly_cost).unwrap_or(false),
            show_routed_cost: d.show_routed_cost.or(def.show_routed_cost).unwrap_or(false),
            show_dashboard_url: d
                .show_dashboard_url
                .or(def.show_dashboard_url)
                .unwrap_or(true),
            dashboard_port: d.dashboard_port,
            dashboard_idle_timeout_sec: d.dashboard_idle_timeout_sec,
            breadcrumb_count: d.breadcrumb_count,
            use_nerd_fonts: d.use_nerd_fonts.or(def.use_nerd_fonts).unwrap_or(false),
            unicode: d.unicode,
            quota_style: d.quota_style,
            terminal_width: d.terminal_width,
            username: normalize_optional_text_for_dto(d.username.as_deref()),
            cache_hit_thresholds: d
                .cache_hit_thresholds
                .clone()
                .filter(|cht| cht.excellent.is_some() || cht.partial.is_some()),
            model_speed_expire_turns: d.model_speed_expire_turns,
            model_speed_expire_minutes: d.model_speed_expire_minutes,
        }
    }
}

impl Default for PulseDisplayDto {
    fn default() -> Self {
        Self::from(&DisplayConfig::builtin_defaults())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PulseConfigDto {
    pub enabled: bool,
    pub language: LanguageSetting,
    pub theme: ThemeConfig,
    pub thresholds: ThresholdsConfig,
    pub display: PulseDisplayDto,
    #[serde(deserialize_with = "deserialize_null_default")]
    pub icons: PulseIconsDto,
}

impl From<&PulseConfig> for PulseConfigDto {
    fn from(cfg: &PulseConfig) -> Self {
        let icons = cfg
            .icons
            .as_ref()
            .map(|ic| PulseIconsDto {
                branch: normalize_optional_text_for_dto(ic.branch.as_deref()),
            })
            .unwrap_or_default();

        Self {
            enabled: cfg.enabled,
            language: cfg.language,
            theme: cfg.theme.clone(),
            thresholds: cfg.thresholds.clone(),
            display: PulseDisplayDto::from(&cfg.display),
            icons,
        }
    }
}

impl Default for PulseConfigDto {
    fn default() -> Self {
        Self::from(&PulseConfig::default())
    }
}

impl From<PulseConfigDto> for PulseConfig {
    fn from(dto: PulseConfigDto) -> Self {
        let d = dto.display;
        let display = DisplayConfig {
            show_context_monitor: Some(d.show_context_monitor),
            show_model_cache: Some(d.show_model_cache),
            show_current_dir: Some(d.show_current_dir),
            show_cycle_mode: Some(d.show_cycle_mode),
            show_speed: Some(d.show_speed),
            show_breadcrumbs: Some(d.show_breadcrumbs),
            show_git_branch: Some(d.show_git_branch),
            show_username: Some(d.show_username),
            show_daily_cost: Some(d.show_daily_cost),
            show_weekly_cost: Some(d.show_weekly_cost),
            show_routed_cost: Some(d.show_routed_cost),
            show_dashboard_url: Some(d.show_dashboard_url),
            dashboard_port: d.dashboard_port,
            dashboard_idle_timeout_sec: d.dashboard_idle_timeout_sec,
            breadcrumb_count: d.breadcrumb_count,
            use_nerd_fonts: Some(d.use_nerd_fonts),
            unicode: d.unicode,
            quota_style: d.quota_style,
            column_width: None,
            terminal_width: d.terminal_width,
            username: normalize_optional_text_preserving_control(d.username),
            cache_hit_thresholds: d
                .cache_hit_thresholds
                .filter(|cht| cht.excellent.is_some() || cht.partial.is_some()),
            model_speed_expire_turns: d.model_speed_expire_turns,
            model_speed_expire_minutes: d.model_speed_expire_minutes,
        };

        let branch = normalize_optional_text_preserving_control(dto.icons.branch);
        let icons = branch.map(|branch| IconsConfig {
            branch: Some(branch),
            ..Default::default()
        });

        Self {
            enabled: dto.enabled,
            theme: dto.theme,
            display,
            thresholds: dto.thresholds,
            language: dto.language,
            icons,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemePresetColorsDto {
    pub primary: &'static str,
    pub secondary: &'static str,
    pub warning: &'static str,
    pub critical: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemePresetDto {
    pub name: &'static str,
    pub colors: ThemePresetColorsDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PulseRuntimeDto {
    pub current_port: u16,
    pub current_idle_timeout_sec: u64,
}

fn build_theme_presets_dto() -> Vec<ThemePresetDto> {
    THEME_PRESETS
        .iter()
        .map(|preset| ThemePresetDto {
            name: preset.name,
            colors: ThemePresetColorsDto {
                primary: preset.primary,
                secondary: preset.secondary,
                warning: preset.warning,
                critical: preset.critical,
            },
        })
        .collect()
}

fn build_runtime_dto(shared: &ServerSharedState) -> PulseRuntimeDto {
    PulseRuntimeDto {
        current_port: shared.port,
        current_idle_timeout_sec: shared.idle_timeout_ms / 1000,
    }
}

fn effective_language_setting(
    config_lang: LanguageSetting,
    env_overrides: &PulseEnvOverrides,
) -> LanguageSetting {
    match env_overrides.language.as_ref().map(|o| o.value.as_str()) {
        Some("zh") => LanguageSetting::Zh,
        Some("en") => LanguageSetting::En,
        Some("auto") => LanguageSetting::Auto,
        _ => config_lang,
    }
}

fn language_to_code(lang: LanguageSetting) -> &'static str {
    match lang {
        LanguageSetting::Zh => "zh",
        LanguageSetting::En | LanguageSetting::Auto => "en",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetPulseConfigResponse {
    version: String,
    file_path: String,
    exists: bool,
    parse_error: Option<String>,
    config: PulseConfigDto,
    defaults: PulseConfigDto,
    theme_presets: Vec<ThemePresetDto>,
    allowed_colors: &'static [&'static str],
    env_overrides: PulseEnvOverrides,
    runtime: PulseRuntimeDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutPulseConfigRequest {
    expected_version: String,
    config: PulseConfigDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PutPulseConfigResponse {
    version: String,
    file_path: String,
    exists: bool,
    parse_error: Option<String>,
    config: PulseConfigDto,
    env_overrides: PulseEnvOverrides,
    restart_required: bool,
    resolved_language: &'static str,
    runtime: PulseRuntimeDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewPulseConfigRequest {
    config: PulseConfigDto,
    #[serde(default)]
    terminal_cols: Option<u16>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewPulseConfigResponse {
    ansi: String,
    resolved_language: &'static str,
}

/// Dispatch `GET`/`PUT /api/config/pulse`.
pub(super) fn handle_pulse_config_route(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
    match req.method.as_str() {
        "GET" => {
            if !check_config_security(stream, req, shared, false) {
                return;
            }
            handle_get_pulse_config(stream, shared);
        }
        "PUT" => {
            if !check_config_security(stream, req, shared, true) {
                return;
            }
            handle_put_pulse_config(stream, req, shared);
        }
        _ => {
            send_config_error(stream, 405, "Method not allowed", None);
        }
    }
}

/// Dispatch `POST /api/config/pulse/preview`.
pub(super) fn handle_pulse_preview_route(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
    if req.method != "POST" {
        send_config_error(stream, 405, "Method not allowed", None);
        return;
    }
    if !check_config_security(stream, req, shared, true) {
        return;
    }
    handle_post_pulse_preview(stream, req);
}

fn handle_get_pulse_config(stream: &mut TcpStream, shared: &ServerSharedState) {
    let snap = match config::read_global_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read global config: {err}"),
                None,
            );
            return;
        }
    };

    let payload = GetPulseConfigResponse {
        version: snap.version,
        file_path: snap.path.to_string_lossy().into_owned(),
        exists: snap.exists,
        parse_error: snap.parse_error,
        config: PulseConfigDto::from(&snap.config),
        defaults: PulseConfigDto::default(),
        theme_presets: build_theme_presets_dto(),
        allowed_colors: ALLOWED_THEME_COLORS,
        env_overrides: config::read_pulse_env_overrides(),
        runtime: build_runtime_dto(shared),
    };
    send_json_value(stream, 200, &payload, &[]);
}

fn handle_put_pulse_config(stream: &mut TcpStream, req: &HttpRequest, shared: &ServerSharedState) {
    let body: PutPulseConfigRequest = match serde_json::from_slice(&req.body) {
        Ok(parsed) => parsed,
        Err(err) => {
            send_config_error(stream, 400, &format!("invalid JSON body: {err}"), None);
            return;
        }
    };

    let Ok(_lock) = shared.pulse_config_lock.lock() else {
        send_config_error(stream, 500, "pulse config lock poisoned", None);
        return;
    };

    let snap = match config::read_global_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read global config: {err}"),
                None,
            );
            return;
        }
    };

    if snap.version != body.expected_version {
        send_json_value(
            stream,
            409,
            &VersionConflictPayload {
                error: "version_conflict",
                current_version: snap.version,
            },
            &[],
        );
        return;
    }

    let candidate = PulseConfig::from(body.config);
    if let Err((field, msg)) = config::validate_pulse_config(&candidate) {
        send_config_error(stream, 400, &msg, Some(&field));
        return;
    }

    let restart_required = snap.config.display.dashboard_port != candidate.display.dashboard_port
        || snap.config.display.dashboard_idle_timeout_sec
            != candidate.display.dashboard_idle_timeout_sec;

    let new_version = match config::save_global_pulse_config_preserving_unknown(
        &snap.path,
        snap.raw_object.as_ref(),
        &candidate,
    ) {
        Ok(v) => v,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to save global config: {err}"),
                None,
            );
            return;
        }
    };

    let saved_config = match config::read_global_config_snapshot_at(&snap.path) {
        Ok(after) if after.parse_error.is_none() => PulseConfigDto::from(&after.config),
        _ => PulseConfigDto::from(&candidate),
    };

    let env_overrides = config::read_pulse_env_overrides();
    let locale = platform::read_locale_env();
    let effective_lang = effective_language_setting(candidate.language, &env_overrides);
    let resolved_lang = resolve_language_from_locale(effective_lang, locale.as_deref());

    let payload = PutPulseConfigResponse {
        version: new_version,
        file_path: snap.path.to_string_lossy().into_owned(),
        exists: true,
        parse_error: None,
        config: saved_config,
        env_overrides,
        restart_required,
        resolved_language: language_to_code(resolved_lang),
        runtime: build_runtime_dto(shared),
    };
    send_json_value(stream, 200, &payload, &[]);
}

fn handle_post_pulse_preview(stream: &mut TcpStream, req: &HttpRequest) {
    let body: PreviewPulseConfigRequest = match serde_json::from_slice(&req.body) {
        Ok(parsed) => parsed,
        Err(err) => {
            send_config_error(stream, 400, &format!("invalid JSON body: {err}"), None);
            return;
        }
    };

    let candidate = PulseConfig::from(body.config);
    if let Err((field, msg)) = config::validate_pulse_config(&candidate) {
        send_config_error(stream, 400, &msg, Some(&field));
        return;
    }

    let cols = body.terminal_cols.map(|c| c.clamp(20, 1000)).unwrap_or(100);
    let unicode_fallback = platform::detect_unicode_support();
    let locale = platform::read_locale_env();
    let ansi = config::render_statusline_preview(
        &candidate,
        unicode_fallback,
        Some(cols),
        locale.as_deref(),
    );
    let resolved_lang = resolve_language_from_locale(candidate.language, locale.as_deref());

    send_json_value(
        stream,
        200,
        &PreviewPulseConfigResponse {
            ansi,
            resolved_language: language_to_code(resolved_lang),
        },
        &[],
    );
}

// ============================================================================
// Sync Config API (/api/config/sync, /api/config/sync/test)
// ============================================================================

fn default_true() -> bool {
    true
}

fn default_push_interval_sec() -> u64 {
    DEFAULT_PUSH_INTERVAL_SEC
}

fn default_sync_prefix() -> String {
    DEFAULT_SYNC_PREFIX.to_string()
}

fn is_secret_configured(val: Option<&str>) -> bool {
    val.map(str::trim).is_some_and(|s| !s.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretStatusDto {
    configured: bool,
    overridden_by_env: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDavSecretStatusDto {
    password: SecretStatusDto,
    token: SecretStatusDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct S3SecretStatusDto {
    access_key: SecretStatusDto,
    secret_key: SecretStatusDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CfWorkerSecretStatusDto {
    token: SecretStatusDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum RemoteViewDto {
    LocalDir {
        path: String,
    },
    #[serde(rename = "webdav")]
    WebDav {
        url: String,
        username: Option<String>,
        #[serde(rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        secrets: WebDavSecretStatusDto,
    },
    S3 {
        endpoint: String,
        region: String,
        bucket: String,
        #[serde(rename = "pathStyle")]
        path_style: bool,
        #[serde(rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        secrets: S3SecretStatusDto,
    },
    CfWorker {
        url: String,
        #[serde(rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        secrets: CfWorkerSecretStatusDto,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncConfigViewDto {
    enabled: bool,
    device_id: String,
    device_name: Option<String>,
    default_account: Option<String>,
    push_interval_sec: u64,
    prefix: String,
    remote: RemoteViewDto,
}

impl SyncConfigViewDto {
    fn from_raw_config(
        cfg: &SyncConfig,
        local_device_id: &str,
        env_overrides: &SyncEnvOverrides,
    ) -> Self {
        let device_id = cfg
            .device_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(local_device_id)
            .to_string();

        let remote = match &cfg.remote {
            RemoteConfig::LocalDir { path } => RemoteViewDto::LocalDir { path: path.clone() },
            RemoteConfig::WebDav {
                url,
                username,
                password,
                token,
                allow_insecure_http,
            } => RemoteViewDto::WebDav {
                url: sanitize_url_for_display(url),
                username: username.clone(),
                allow_insecure_http: *allow_insecure_http,
                secrets: WebDavSecretStatusDto {
                    password: SecretStatusDto {
                        configured: is_secret_configured(password.as_deref()),
                        overridden_by_env: env_overrides.password,
                    },
                    token: SecretStatusDto {
                        configured: is_secret_configured(token.as_deref()),
                        overridden_by_env: env_overrides.token,
                    },
                },
            },
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                access_key,
                secret_key,
                path_style,
                allow_insecure_http,
            } => RemoteViewDto::S3 {
                endpoint: sanitize_url_for_display(endpoint),
                region: region.clone(),
                bucket: bucket.clone(),
                path_style: *path_style,
                allow_insecure_http: *allow_insecure_http,
                secrets: S3SecretStatusDto {
                    access_key: SecretStatusDto {
                        configured: is_secret_configured(access_key.as_deref()),
                        overridden_by_env: env_overrides.access_key,
                    },
                    secret_key: SecretStatusDto {
                        configured: is_secret_configured(secret_key.as_deref()),
                        overridden_by_env: env_overrides.secret_key,
                    },
                },
            },
            RemoteConfig::CfWorker {
                url,
                token,
                allow_insecure_http,
            } => RemoteViewDto::CfWorker {
                url: sanitize_url_for_display(url),
                allow_insecure_http: *allow_insecure_http,
                secrets: CfWorkerSecretStatusDto {
                    token: SecretStatusDto {
                        configured: is_secret_configured(token.as_deref()),
                        overridden_by_env: env_overrides.token,
                    },
                },
            },
        };

        Self {
            enabled: cfg.enabled,
            device_id,
            device_name: cfg.device_name.clone(),
            default_account: cfg.default_account.clone(),
            push_interval_sec: cfg.push_interval_sec,
            prefix: cfg.prefix.clone(),
            remote,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SecretMutationMode {
    #[default]
    Keep,
    Set,
    Clear,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SecretMutationDto {
    #[serde(default)]
    mode: SecretMutationMode,
    value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WebDavSecretWriteDto {
    password: SecretMutationDto,
    token: SecretMutationDto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct S3SecretWriteDto {
    access_key: SecretMutationDto,
    secret_key: SecretMutationDto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CfWorkerSecretWriteDto {
    token: SecretMutationDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum RemoteWriteDto {
    LocalDir {
        #[serde(default)]
        path: String,
    },
    #[serde(rename = "webdav", alias = "web-dav")]
    WebDav {
        #[serde(default)]
        url: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default, rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        #[serde(default, deserialize_with = "deserialize_null_default")]
        secrets: WebDavSecretWriteDto,
    },
    S3 {
        #[serde(default)]
        endpoint: String,
        #[serde(default)]
        region: String,
        #[serde(default)]
        bucket: String,
        #[serde(default = "default_true", rename = "pathStyle")]
        path_style: bool,
        #[serde(default, rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        #[serde(default, deserialize_with = "deserialize_null_default")]
        secrets: S3SecretWriteDto,
    },
    CfWorker {
        #[serde(default)]
        url: String,
        #[serde(default, rename = "allowInsecureHttp")]
        allow_insecure_http: bool,
        #[serde(default, deserialize_with = "deserialize_null_default")]
        secrets: CfWorkerSecretWriteDto,
    },
}

impl Default for RemoteWriteDto {
    fn default() -> Self {
        Self::LocalDir {
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SyncConfigWriteDto {
    enabled: bool,
    device_name: Option<String>,
    default_account: Option<String>,
    #[serde(default = "default_push_interval_sec")]
    push_interval_sec: u64,
    #[serde(default = "default_sync_prefix")]
    prefix: String,
    remote: RemoteWriteDto,
}

impl Default for SyncConfigWriteDto {
    fn default() -> Self {
        Self {
            enabled: false,
            device_name: None,
            default_account: None,
            push_interval_sec: DEFAULT_PUSH_INTERVAL_SEC,
            prefix: DEFAULT_SYNC_PREFIX.to_string(),
            remote: RemoteWriteDto::default(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetSyncConfigResponse {
    version: String,
    file_path: String,
    exists: bool,
    parse_error: Option<String>,
    local_device_id: String,
    config: SyncConfigViewDto,
    env_overrides: SyncEnvOverrides,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutSyncConfigRequest {
    expected_version: String,
    config: SyncConfigWriteDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PutSyncConfigResponse {
    version: String,
    file_path: String,
    exists: bool,
    parse_error: Option<String>,
    local_device_id: String,
    config: SyncConfigViewDto,
    env_overrides: SyncEnvOverrides,
    remote_changed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestSyncConfigRequest {
    config: SyncConfigWriteDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestSyncConfigResponse {
    ok: bool,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    latency_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteSyncConfigRequest {
    expected_version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteSyncConfigResponse {
    deleted: bool,
    version: &'static str,
}

fn read_local_device_id(shared: &ServerSharedState) -> Result<String, String> {
    Ok(shared.local_device_id.clone())
}

fn validate_and_normalize_prefix(raw: &str) -> Result<String, (String, String)> {
    if raw.chars().any(|c| c.is_control() || c == '\0') {
        return Err((
            "prefix".to_string(),
            "prefix must not contain control characters".to_string(),
        ));
    }
    if raw.contains('\\') {
        return Err((
            "prefix".to_string(),
            "prefix must not contain '\\'".to_string(),
        ));
    }
    let trimmed = raw.trim().trim_matches('/');
    if !trimmed.is_empty() {
        for seg in trimmed.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return Err((
                    "prefix".to_string(),
                    "prefix must not contain empty, '.', or '..' path segments".to_string(),
                ));
            }
        }
    }
    Ok(normalize_prefix(raw))
}

fn resolve_secret_mutation(
    mutation: &SecretMutationDto,
    existing: Option<&str>,
    field: &str,
) -> Result<Option<String>, (String, String)> {
    match mutation.mode {
        SecretMutationMode::Keep => Ok(existing
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)),
        SecretMutationMode::Clear => Ok(None),
        SecretMutationMode::Set => {
            let raw = mutation.value.as_deref().unwrap_or("");
            if raw.chars().any(char::is_control) {
                return Err((
                    field.to_string(),
                    format!("{field} must not contain control characters"),
                ));
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err((
                    field.to_string(),
                    format!("{field} must not be empty when mode is 'set'"),
                ));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

fn merge_remote_write(
    write: RemoteWriteDto,
    existing_raw: Option<&SyncConfig>,
) -> Result<RemoteConfig, (String, String)> {
    match write {
        RemoteWriteDto::LocalDir { path } => {
            if path.chars().any(char::is_control) {
                return Err((
                    "remote.path".to_string(),
                    "remote.path must not contain control characters".to_string(),
                ));
            }
            Ok(RemoteConfig::LocalDir {
                path: path.trim().to_string(),
            })
        }
        RemoteWriteDto::WebDav {
            url,
            username,
            allow_insecure_http,
            secrets,
        } => {
            if url.chars().any(char::is_control) {
                return Err((
                    "remote.url".to_string(),
                    "remote.url must not contain control characters".to_string(),
                ));
            }
            if username
                .as_deref()
                .is_some_and(|u| u.chars().any(char::is_control))
            {
                return Err((
                    "remote.username".to_string(),
                    "remote.username must not contain control characters".to_string(),
                ));
            }
            let (prev_password, prev_token) = match existing_raw.map(|c| &c.remote) {
                Some(RemoteConfig::WebDav {
                    password, token, ..
                }) => (password.as_deref(), token.as_deref()),
                _ => (None, None),
            };
            let password = resolve_secret_mutation(
                &secrets.password,
                prev_password,
                "remote.secrets.password",
            )?;
            let token =
                resolve_secret_mutation(&secrets.token, prev_token, "remote.secrets.token")?;
            Ok(RemoteConfig::WebDav {
                url: url.trim().to_string(),
                username: username
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty()),
                password,
                token,
                allow_insecure_http,
            })
        }
        RemoteWriteDto::S3 {
            endpoint,
            region,
            bucket,
            path_style,
            allow_insecure_http,
            secrets,
        } => {
            if endpoint.chars().any(char::is_control) {
                return Err((
                    "remote.endpoint".to_string(),
                    "remote.endpoint must not contain control characters".to_string(),
                ));
            }
            if region.chars().any(char::is_control) {
                return Err((
                    "remote.region".to_string(),
                    "remote.region must not contain control characters".to_string(),
                ));
            }
            if bucket.chars().any(char::is_control) {
                return Err((
                    "remote.bucket".to_string(),
                    "remote.bucket must not contain control characters".to_string(),
                ));
            }
            let (prev_ak, prev_sk) = match existing_raw.map(|c| &c.remote) {
                Some(RemoteConfig::S3 {
                    access_key,
                    secret_key,
                    ..
                }) => (access_key.as_deref(), secret_key.as_deref()),
                _ => (None, None),
            };
            let access_key =
                resolve_secret_mutation(&secrets.access_key, prev_ak, "remote.secrets.accessKey")?;
            let secret_key =
                resolve_secret_mutation(&secrets.secret_key, prev_sk, "remote.secrets.secretKey")?;
            let trimmed_region = region.trim();
            Ok(RemoteConfig::S3 {
                endpoint: endpoint.trim().to_string(),
                region: if trimmed_region.is_empty() {
                    "auto".to_string()
                } else {
                    trimmed_region.to_string()
                },
                bucket: bucket.trim().to_string(),
                access_key,
                secret_key,
                path_style,
                allow_insecure_http,
            })
        }
        RemoteWriteDto::CfWorker {
            url,
            allow_insecure_http,
            secrets,
        } => {
            if url.chars().any(char::is_control) {
                return Err((
                    "remote.url".to_string(),
                    "remote.url must not contain control characters".to_string(),
                ));
            }
            let prev_token = match existing_raw.map(|c| &c.remote) {
                Some(RemoteConfig::CfWorker { token, .. }) => token.as_deref(),
                _ => None,
            };
            let token =
                resolve_secret_mutation(&secrets.token, prev_token, "remote.secrets.token")?;
            Ok(RemoteConfig::CfWorker {
                url: url.trim().to_string(),
                token,
                allow_insecure_http,
            })
        }
    }
}

struct ValidatedScalarSyncFields {
    default_account: Option<String>,
    device_name: Option<String>,
    prefix: String,
}

fn validate_scalar_sync_fields(
    write: &SyncConfigWriteDto,
) -> Result<ValidatedScalarSyncFields, (String, String)> {
    let default_account = match write.default_account.as_deref() {
        None => None,
        Some(raw) => {
            if raw.chars().any(char::is_control) {
                return Err((
                    "defaultAccount".to_string(),
                    "defaultAccount must not contain control characters".to_string(),
                ));
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                let Some(norm) = validate_and_normalize_account_email(trimmed) else {
                    return Err((
                        "defaultAccount".to_string(),
                        "defaultAccount must be a valid email address".to_string(),
                    ));
                };
                if norm == UNASSIGNED_ACCOUNT_ID {
                    return Err((
                        "defaultAccount".to_string(),
                        "defaultAccount must not be 'unassigned'".to_string(),
                    ));
                }
                Some(norm)
            }
        }
    };

    let device_name = match write.device_name.as_deref() {
        None => None,
        Some(raw) => {
            if raw.chars().any(char::is_control) {
                return Err((
                    "deviceName".to_string(),
                    "deviceName must not contain control characters".to_string(),
                ));
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > 128 {
                return Err((
                    "deviceName".to_string(),
                    "deviceName must be at most 128 characters".to_string(),
                ));
            } else {
                platform::sanitize_device_name(trimmed)
            }
        }
    };

    if !(1..=86_400).contains(&write.push_interval_sec) {
        return Err((
            "pushIntervalSec".to_string(),
            "pushIntervalSec must be between 1 and 86400".to_string(),
        ));
    }

    let prefix = validate_and_normalize_prefix(&write.prefix)?;
    Ok(ValidatedScalarSyncFields {
        default_account,
        device_name,
        prefix,
    })
}

fn infer_sync_remote_error_field(remote: &RemoteConfig, msg: &str) -> &'static str {
    match remote {
        RemoteConfig::LocalDir { .. } => "remote.path",
        RemoteConfig::WebDav { .. } => "remote.url",
        RemoteConfig::S3 { .. } => {
            if msg.contains("accessKey") {
                "remote.secrets.accessKey"
            } else if msg.contains("secretKey") {
                "remote.secrets.secretKey"
            } else if msg.contains("bucket") {
                "remote.bucket"
            } else {
                "remote.endpoint"
            }
        }
        RemoteConfig::CfWorker { .. } => {
            if msg.contains("token") {
                "remote.secrets.token"
            } else {
                "remote.url"
            }
        }
    }
}

fn validate_sync_config_effective(to_save: &SyncConfig) -> Result<(), (String, String)> {
    let mut effective = to_save.clone();
    effective.apply_env_overrides();

    let summary = effective.to_summary();
    if effective.enabled {
        if let Err(err) = sync::build_remote(&effective) {
            let redacted = redact_sync_error_message(&err.to_string(), Some(&summary));
            let field = infer_sync_remote_error_field(&effective.remote, &redacted);
            return Err((field.to_string(), redacted));
        }
        return Ok(());
    }

    // When disabled, validate non-empty URL / S3 bucket without blocking on missing secrets.
    match &effective.remote {
        RemoteConfig::LocalDir { .. } => {}
        RemoteConfig::WebDav {
            url,
            allow_insecure_http,
            ..
        }
        | RemoteConfig::CfWorker {
            url,
            allow_insecure_http,
            ..
        } => {
            if !url.trim().is_empty()
                && let Err(err) = validate_remote_url(url, *allow_insecure_http)
            {
                return Err((
                    "remote.url".to_string(),
                    redact_sync_error_message(&err.to_string(), Some(&summary)),
                ));
            }
        }
        RemoteConfig::S3 {
            endpoint,
            bucket,
            allow_insecure_http,
            ..
        } => {
            if !endpoint.trim().is_empty()
                && let Err(err) = validate_remote_url(endpoint, *allow_insecure_http)
            {
                return Err((
                    "remote.endpoint".to_string(),
                    redact_sync_error_message(&err.to_string(), Some(&summary)),
                ));
            }
            if !bucket.trim().is_empty()
                && let Err(err) = validate_single_segment(bucket.trim(), "s3 bucket")
            {
                return Err((
                    "remote.bucket".to_string(),
                    redact_sync_error_message(&err.to_string(), Some(&summary)),
                ));
            }
        }
    }
    Ok(())
}

/// Dispatch `GET`/`PUT`/`DELETE /api/config/sync`.
pub(super) fn handle_sync_config_route(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
    match req.method.as_str() {
        "GET" => {
            if !check_config_security(stream, req, shared, false) {
                return;
            }
            handle_get_sync_config(stream, shared);
        }
        "PUT" => {
            if !check_config_security(stream, req, shared, true) {
                return;
            }
            handle_put_sync_config(stream, req, shared);
        }
        "DELETE" => {
            if !check_config_security(stream, req, shared, true) {
                return;
            }
            handle_delete_sync_config(stream, req, shared);
        }
        _ => {
            send_config_error(stream, 405, "Method not allowed", None);
        }
    }
}

/// Dispatch `POST /api/config/sync/test`.
pub(super) fn handle_sync_test_route(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
    if req.method != "POST" {
        send_config_error(stream, 405, "Method not allowed", None);
        return;
    }
    if !check_config_security(stream, req, shared, true) {
        return;
    }
    handle_post_sync_test(stream, req);
}

fn handle_get_sync_config(stream: &mut TcpStream, shared: &ServerSharedState) {
    let local_device_id = match read_local_device_id(shared) {
        Ok(id) => id,
        Err(msg) => {
            send_config_error(stream, 500, &msg, None);
            return;
        }
    };

    let snap = match sync::read_raw_sync_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read sync config: {err}"),
                None,
            );
            return;
        }
    };

    let env_overrides = sync::active_sync_env_overrides();
    let dto = SyncConfigViewDto::from_raw_config(&snap.config, &local_device_id, &env_overrides);

    send_json_value(
        stream,
        200,
        &GetSyncConfigResponse {
            version: snap.version,
            file_path: snap.path.to_string_lossy().into_owned(),
            exists: snap.exists,
            parse_error: snap.parse_error,
            local_device_id,
            config: dto,
            env_overrides,
        },
        &[],
    );
}

fn handle_put_sync_config(stream: &mut TcpStream, req: &HttpRequest, shared: &ServerSharedState) {
    let body: PutSyncConfigRequest = match serde_json::from_slice(&req.body) {
        Ok(parsed) => parsed,
        Err(err) => {
            send_config_error(stream, 400, &format!("invalid JSON body: {err}"), None);
            return;
        }
    };

    let Ok(_lock) = shared.sync_config_lock.lock() else {
        send_config_error(stream, 500, "sync config lock poisoned", None);
        return;
    };

    let snap = match sync::read_raw_sync_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read sync config: {err}"),
                None,
            );
            return;
        }
    };

    if snap.version != body.expected_version {
        send_json_value(
            stream,
            409,
            &VersionConflictPayload {
                error: "version_conflict",
                current_version: snap.version,
            },
            &[],
        );
        return;
    }

    let local_device_id = match read_local_device_id(shared) {
        Ok(id) => id,
        Err(msg) => {
            send_config_error(stream, 500, &msg, None);
            return;
        }
    };

    let existing_raw = if snap.exists && snap.parse_error.is_none() {
        Some(&snap.config)
    } else {
        None
    };

    let scalar = match validate_scalar_sync_fields(&body.config) {
        Ok(v) => v,
        Err((field, msg)) => {
            send_config_error(stream, 400, &msg, Some(&field));
            return;
        }
    };
    let enabled = body.config.enabled;
    let push_interval_sec = body.config.push_interval_sec;
    let remote = match merge_remote_write(body.config.remote, existing_raw) {
        Ok(r) => r,
        Err((field, msg)) => {
            send_config_error(stream, 400, &msg, Some(&field));
            return;
        }
    };

    let device_id = existing_raw
        .and_then(|c| c.device_id.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(local_device_id.clone()));

    let mut to_save = SyncConfig {
        enabled,
        device_id,
        device_name: scalar.device_name,
        default_account: scalar.default_account,
        push_interval_sec,
        prefix: scalar.prefix,
        remote,
    };
    to_save.normalize();

    if let Err((field, msg)) = validate_sync_config_effective(&to_save) {
        send_config_error(stream, 400, &msg, Some(&field));
        return;
    }

    if let Err(err) = sync::save_sync_config_to(&snap.path, &to_save) {
        let summary = to_save.to_summary();
        send_config_error(
            stream,
            500,
            &format!(
                "failed to save sync config: {}",
                redact_sync_error_message(&err.to_string(), Some(&summary))
            ),
            None,
        );
        return;
    }

    let remote_changed =
        existing_raw.map(SyncConfig::remote_fingerprint) != Some(to_save.remote_fingerprint());

    if let Ok(mut opts) = shared.sync_options.lock() {
        refresh_sync_identity_hints(&mut opts);
    }

    let after = match sync::read_raw_sync_config_snapshot_at(&snap.path) {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to re-read saved sync config: {err}"),
                None,
            );
            return;
        }
    };

    let env_overrides = sync::active_sync_env_overrides();
    let dto = SyncConfigViewDto::from_raw_config(&after.config, &local_device_id, &env_overrides);

    send_json_value(
        stream,
        200,
        &PutSyncConfigResponse {
            version: after.version,
            file_path: after.path.to_string_lossy().into_owned(),
            exists: after.exists,
            parse_error: after.parse_error,
            local_device_id,
            config: dto,
            env_overrides,
            remote_changed,
        },
        &[],
    );
}

fn handle_post_sync_test(stream: &mut TcpStream, req: &HttpRequest) {
    let body: TestSyncConfigRequest = match serde_json::from_slice(&req.body) {
        Ok(parsed) => parsed,
        Err(err) => {
            send_config_error(stream, 400, &format!("invalid JSON body: {err}"), None);
            return;
        }
    };

    let prefix = match validate_and_normalize_prefix(&body.config.prefix) {
        Ok(p) => p,
        Err((field, msg)) => {
            send_config_error(stream, 400, &msg, Some(&field));
            return;
        }
    };

    let snap = match sync::read_raw_sync_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read sync config: {err}"),
                None,
            );
            return;
        }
    };
    let existing_raw = if snap.exists && snap.parse_error.is_none() {
        Some(&snap.config)
    } else {
        None
    };

    let remote = match merge_remote_write(body.config.remote, existing_raw) {
        Ok(r) => r,
        Err((field, msg)) => {
            send_config_error(stream, 400, &msg, Some(&field));
            return;
        }
    };

    let mut effective = SyncConfig {
        enabled: true,
        device_id: existing_raw.and_then(|c| c.device_id.clone()),
        device_name: body.config.device_name,
        default_account: body.config.default_account,
        push_interval_sec: body.config.push_interval_sec.max(1),
        prefix,
        remote,
    };
    effective.normalize();
    effective.apply_env_overrides();

    let target = effective.redacted_target();
    let started = Instant::now();
    let outcome = sync::build_remote(&effective).and_then(|remote| remote.probe());
    let latency_ms = started.elapsed().as_millis() as u64;

    match outcome {
        Ok(info) => {
            send_json_value(
                stream,
                200,
                &TestSyncConfigResponse {
                    ok: true,
                    target,
                    message: Some(format!("{} reachable: {}", info.backend, info.detail)),
                    error: None,
                    latency_ms,
                },
                &[],
            );
        }
        Err(err) => {
            let summary = effective.to_summary();
            let redacted = redact_sync_error_message(&err.to_string(), Some(&summary));
            send_json_value(
                stream,
                200,
                &TestSyncConfigResponse {
                    ok: false,
                    target,
                    message: None,
                    error: Some(redacted),
                    latency_ms,
                },
                &[],
            );
        }
    }
}

fn handle_delete_sync_config(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
    let body: DeleteSyncConfigRequest = match serde_json::from_slice(&req.body) {
        Ok(parsed) => parsed,
        Err(err) => {
            send_config_error(stream, 400, &format!("invalid JSON body: {err}"), None);
            return;
        }
    };

    let Ok(_lock) = shared.sync_config_lock.lock() else {
        send_config_error(stream, 500, "sync config lock poisoned", None);
        return;
    };

    let snap = match sync::read_raw_sync_config_snapshot() {
        Ok(s) => s,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to read sync config: {err}"),
                None,
            );
            return;
        }
    };

    if snap.version != body.expected_version {
        send_json_value(
            stream,
            409,
            &VersionConflictPayload {
                error: "version_conflict",
                current_version: snap.version,
            },
            &[],
        );
        return;
    }

    let deleted = match sync::delete_sync_config_at(&snap.path) {
        Ok(d) => d,
        Err(err) => {
            send_config_error(
                stream,
                500,
                &format!("failed to delete sync config: {err}"),
                None,
            );
            return;
        }
    };

    if let Ok(mut opts) = shared.sync_options.lock() {
        refresh_sync_identity_hints(&mut opts);
    }

    send_json_value(
        stream,
        200,
        &DeleteSyncConfigResponse {
            deleted,
            version: "absent",
        },
        &[],
    );
}
