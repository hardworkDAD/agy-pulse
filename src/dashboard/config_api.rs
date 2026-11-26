use std::net::TcpStream;

use serde::{Deserialize, Deserializer, Serialize};

use crate::config::{
    self, ALLOWED_THEME_COLORS, CacheHitThresholdsConfig, DisplayConfig, IconsConfig,
    LanguageSetting, PulseConfig, PulseEnvOverrides, QuotaStyle, THEME_PRESETS, ThemeConfig,
    ThresholdsConfig,
};
use crate::platform;
use crate::render::resolve_language_from_locale;

use super::http_wire::{
    HttpRequest, constant_time_eq, is_json_content_type, is_strict_same_origin,
};
use super::server::{ServerSharedState, send_json_value};

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
    let valid_csrf = req.header("x-agy-pulse-token").is_some_and(|token| {
        constant_time_eq(token.as_bytes(), shared.csrf_token.as_bytes())
    });
    if !valid_csrf {
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
    pub token: Option<String>,
    pub ctx: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PulseDisplayDto {
    pub show_context_monitor: bool,
    pub show_model_cache: bool,
    pub show_current_dir: bool,
    pub show_cycle_mode: bool,
    pub show_speed: bool,
    pub show_model: bool,
    pub show_token_bar: bool,
    pub show_context_bar: bool,
    pub show_turn_cache_hit_rate: bool,
    pub show_breadcrumbs: bool,
    pub show_git_branch: bool,
    pub show_username: bool,
    pub show_cost: bool,
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
            show_model: d.show_model.or(def.show_model).unwrap_or(false),
            show_token_bar: d.show_token_bar.or(def.show_token_bar).unwrap_or(false),
            show_context_bar: d.show_context_bar.or(def.show_context_bar).unwrap_or(false),
            show_turn_cache_hit_rate: d
                .show_turn_cache_hit_rate
                .or(def.show_turn_cache_hit_rate)
                .unwrap_or(false),
            show_breadcrumbs: d.show_breadcrumbs.or(def.show_breadcrumbs).unwrap_or(false),
            show_git_branch: d.show_git_branch.or(def.show_git_branch).unwrap_or(false),
            show_username: d.show_username.or(def.show_username).unwrap_or(false),
            show_cost: d.show_cost.or(def.show_cost).unwrap_or(false),
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
                token: normalize_optional_text_for_dto(ic.token.as_deref()),
                ctx: normalize_optional_text_for_dto(ic.ctx.as_deref()),
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
            show_model: Some(d.show_model),
            show_token_bar: Some(d.show_token_bar),
            show_context_bar: Some(d.show_context_bar),
            show_turn_cache_hit_rate: Some(d.show_turn_cache_hit_rate),
            show_breadcrumbs: Some(d.show_breadcrumbs),
            show_git_branch: Some(d.show_git_branch),
            show_username: Some(d.show_username),
            show_cost: Some(d.show_cost),
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
        let token = normalize_optional_text_preserving_control(dto.icons.token);
        let ctx = normalize_optional_text_preserving_control(dto.icons.ctx);
        let icons = if branch.is_none() && token.is_none() && ctx.is_none() {
            None
        } else {
            Some(IconsConfig {
                branch,
                token,
                ctx,
                ..Default::default()
            })
        };

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

fn handle_put_pulse_config(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) {
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

    let cols = body
        .terminal_cols
        .map(|c| c.clamp(20, 1000))
        .unwrap_or(100);
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
