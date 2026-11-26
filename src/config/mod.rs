pub mod wizard;

pub use wizard::{
    WizardRunOptions, WizardRunOutcome, render_statusline_preview, run_config_cli, run_wizard_loop,
};

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use unicode_width::UnicodeWidthStr;

use crate::platform;

/// Version string returned when a configuration file does not yet exist on disk.
pub const ABSENT_CONFIG_VERSION: &str = "absent";

/// Canonical ANSI color names allowed for statusline theme customization.
pub const ALLOWED_THEME_COLORS: &[&str] = &[
    "green", "gray", "blue", "magenta", "yellow", "cyan", "red",
];

/// Built-in statusline theme preset definition shared by wizard and dashboard config API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ThemePresetDef {
    pub name: &'static str,
    pub primary: &'static str,
    pub secondary: &'static str,
    pub warning: &'static str,
    pub critical: &'static str,
}

/// Built-in statusline theme presets.
pub const THEME_PRESETS: &[ThemePresetDef] = &[
    ThemePresetDef {
        name: "Emerald Green",
        primary: "green",
        secondary: "gray",
        warning: "yellow",
        critical: "red",
    },
    ThemePresetDef {
        name: "Ocean Blue",
        primary: "blue",
        secondary: "gray",
        warning: "yellow",
        critical: "red",
    },
    ThemePresetDef {
        name: "Cyberpunk Magenta",
        primary: "magenta",
        secondary: "gray",
        warning: "yellow",
        critical: "red",
    },
    ThemePresetDef {
        name: "Amber Gold",
        primary: "yellow",
        secondary: "gray",
        warning: "cyan",
        critical: "red",
    },
];

/// Single active environment variable override affecting runtime Pulse behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PulseEnvOverride {
    pub env_var: &'static str,
    pub value: String,
}

/// Active environment overrides for Pulse configuration (metadata only, never persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PulseEnvOverrides {
    pub language: Option<PulseEnvOverride>,
    pub dashboard_port: Option<PulseEnvOverride>,
    pub dashboard_idle_timeout_sec: Option<PulseEnvOverride>,
    pub unicode: Option<PulseEnvOverride>,
}

/// Snapshot of the global `agy-pulse.config.json` without environment overrides.
#[derive(Debug, Clone)]
pub struct GlobalPulseConfigSnapshot {
    pub path: PathBuf,
    pub exists: bool,
    pub version: String,
    pub raw_object: Option<Map<String, Value>>,
    pub parse_error: Option<String>,
    pub config: PulseConfig,
}

/// Language configuration preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageSetting {
    #[default]
    Auto,
    En,
    Zh,
}

/// Quota display mode; retired v1 `"models"` values smoothly fall back to `Table`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaStyle {
    #[default]
    Table,
    Compact,
    None,
}

impl<'de> Deserialize<'de> for QuotaStyle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Option::<String>::deserialize(deserializer)?;
        match raw.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("compact") => Ok(Self::Compact),
            Some("none") => Ok(Self::None),
            _ => Ok(Self::Table),
        }
    }
}

/// Theme colors for statusline rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub primary: String,
    pub secondary: String,
    pub warning: String,
    pub critical: String,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            primary: "green".to_string(),
            secondary: "gray".to_string(),
            warning: "yellow".to_string(),
            critical: "red".to_string(),
        }
    }
}

/// Theme configuration accepting either a preset/color string or a 4-color object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ThemeConfig {
    Preset(String),
    Colors(ThemeColors),
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self::Colors(ThemeColors::default())
    }
}

impl<'de> Deserialize<'de> for ThemeConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        match val {
            Value::String(s) => Ok(Self::Preset(s)),
            Value::Object(_) => Ok(Self::Colors(
                serde_json::from_value(val).unwrap_or_default(),
            )),
            _ => Ok(Self::default()),
        }
    }
}

/// Optional custom glyph overrides for statusline segments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IconsConfig {
    pub branch: Option<String>,
    pub plan: Option<String>,
    pub step: Option<String>,
    pub task: Option<String>,
    pub token: Option<String>,
    pub ctx: Option<String>,
    pub model: Option<String>,
}

/// Threshold fractions (0.0..=1.0) for warning and critical state coloring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThresholdsConfig {
    pub warning: f64,
    pub critical: f64,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            warning: 0.7,
            critical: 0.9,
        }
    }
}

/// Threshold percentages (0..=100) for per-turn cache hit rate coloring.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheHitThresholdsConfig {
    pub excellent: Option<f64>,
    pub partial: Option<f64>,
}

/// Display flags and formatting options compatible with v1 `agy-pulse.config.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DisplayConfig {
    pub show_context_monitor: Option<bool>,
    pub show_model_cache: Option<bool>,
    pub show_current_dir: Option<bool>,
    pub show_cycle_mode: Option<bool>,
    pub show_speed: Option<bool>,
    pub show_model: Option<bool>,
    pub show_token_bar: Option<bool>,
    pub show_context_bar: Option<bool>,
    pub show_turn_cache_hit_rate: Option<bool>,
    pub show_breadcrumbs: Option<bool>,
    pub show_git_branch: Option<bool>,
    pub show_username: Option<bool>,
    pub show_cost: Option<bool>,
    pub show_daily_cost: Option<bool>,
    pub show_weekly_cost: Option<bool>,
    pub show_routed_cost: Option<bool>,
    pub show_dashboard_url: Option<bool>,
    pub dashboard_port: Option<u16>,
    pub dashboard_idle_timeout_sec: Option<u64>,
    pub breadcrumb_count: Option<u32>,
    pub use_nerd_fonts: Option<bool>,
    pub unicode: Option<bool>,
    pub quota_style: QuotaStyle,
    pub column_width: Option<u16>,
    pub terminal_width: Option<u16>,
    pub username: Option<String>,
    pub cache_hit_thresholds: Option<CacheHitThresholdsConfig>,
    pub model_speed_expire_turns: Option<u64>,
    pub model_speed_expire_minutes: Option<f64>,
}

impl DisplayConfig {
    /// Built-in defaults matching `runtime/agy-pulse.config.json`.
    pub fn builtin_defaults() -> Self {
        Self {
            show_context_monitor: Some(true),
            show_model_cache: Some(true),
            show_current_dir: Some(true),
            show_cycle_mode: Some(true),
            show_speed: Some(true),
            show_model: Some(false),
            show_token_bar: Some(false),
            show_context_bar: Some(false),
            show_turn_cache_hit_rate: Some(false),
            show_breadcrumbs: Some(false),
            show_git_branch: Some(false),
            show_username: Some(false),
            show_cost: Some(false),
            show_daily_cost: Some(false),
            show_weekly_cost: Some(false),
            show_routed_cost: Some(false),
            show_dashboard_url: Some(true),
            dashboard_port: None,
            dashboard_idle_timeout_sec: None,
            breadcrumb_count: Some(3),
            use_nerd_fonts: Some(false),
            unicode: None,
            quota_style: QuotaStyle::Table,
            column_width: None,
            terminal_width: None,
            username: None,
            cache_hit_thresholds: None,
            model_speed_expire_turns: None,
            model_speed_expire_minutes: None,
        }
    }
}

/// Complete user configuration merged from built-in defaults, global file, workspace file, and env.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PulseConfig {
    pub enabled: bool,
    pub theme: ThemeConfig,
    pub display: DisplayConfig,
    pub thresholds: ThresholdsConfig,
    pub language: LanguageSetting,
    pub icons: Option<IconsConfig>,
}

impl Default for PulseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: ThemeConfig::default(),
            display: DisplayConfig::builtin_defaults(),
            thresholds: ThresholdsConfig::default(),
            language: LanguageSetting::Auto,
            icons: None,
        }
    }
}

pub(crate) fn read_json_object(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    if val.is_object() { Some(val) } else { None }
}

pub(crate) fn deep_merge_value(base: Value, override_val: Value) -> Value {
    match (base, override_val) {
        (Value::Object(mut base_map), Value::Object(override_map)) => {
            for (k, v) in override_map {
                let merged = match base_map.remove(&k) {
                    Some(existing) => deep_merge_value(existing, v),
                    None => v,
                };
                base_map.insert(k, merged);
            }
            Value::Object(base_map)
        }
        (_, override_leaf) => override_leaf,
    }
}

fn apply_env_overrides(merged: &mut Value) {
    if let Some(lang_raw) = platform::read_language_env_override() {
        let lower = lang_raw.to_ascii_lowercase();
        let lang_val = match lower.as_str() {
            "zh" | "en" | "auto" => Some(Value::String(lower)),
            _ => None,
        };
        if let (Some(val), Value::Object(map)) = (lang_val, merged) {
            map.insert("language".to_string(), val);
        }
    }
}

/// Load configuration according to precedence:
/// Built-in defaults -> Global config -> Workspace config -> Environment overrides.
/// Unknown or retired v1 fields are silently ignored.
pub fn load_config(workspace_dir: &Path) -> PulseConfig {
    let mut merged = serde_json::to_value(PulseConfig::default())
        .unwrap_or_else(|_| Value::Object(Map::new()));

    let global_path = platform::resolve_global_config_dir().join("agy-pulse.config.json");
    if let Some(global_val) = read_json_object(&global_path) {
        merged = deep_merge_value(merged, global_val);
    }

    let local_path = workspace_dir.join("agy-pulse.config.json");
    let same_file = fs::canonicalize(&local_path)
        .ok()
        .zip(fs::canonicalize(&global_path).ok())
        .is_some_and(|(a, b)| a == b);
    if !same_file && let Some(local_val) = read_json_object(&local_path) {
        merged = deep_merge_value(merged, local_val);
    }

    apply_env_overrides(&mut merged);

    serde_json::from_value(merged).unwrap_or_default()
}

/// Resolve the global `agy-pulse.config.json` path.
pub fn resolve_global_config_path() -> PathBuf {
    platform::resolve_global_config_dir().join("agy-pulse.config.json")
}

/// Read currently active Pulse environment overrides without mutating saved configuration.
pub fn read_pulse_env_overrides() -> PulseEnvOverrides {
    let language = platform::read_language_env_override().and_then(|raw| {
        let lower = raw.to_ascii_lowercase();
        match lower.as_str() {
            "zh" | "en" | "auto" => Some(PulseEnvOverride {
                env_var: "AGY_PULSE_LANGUAGE",
                value: lower,
            }),
            _ => None,
        }
    });

    let dashboard_port = env::var("AGY_PULSE_DASHBOARD_PORT")
        .ok()
        .and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed
                .parse::<u16>()
                .ok()
                .filter(|&port| port >= 1)
                .map(|port| PulseEnvOverride {
                    env_var: "AGY_PULSE_DASHBOARD_PORT",
                    value: port.to_string(),
                })
        });

    let dashboard_idle_timeout_sec = env::var("AGY_PULSE_DASHBOARD_IDLE_SEC")
        .ok()
        .and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.parse::<u64>().ok().map(|sec| PulseEnvOverride {
                env_var: "AGY_PULSE_DASHBOARD_IDLE_SEC",
                value: sec.to_string(),
            })
        });

    let unicode = if env::var("AGY_PULSE_FORCE_ASCII").ok().as_deref() == Some("1") {
        Some(PulseEnvOverride {
            env_var: "AGY_PULSE_FORCE_ASCII",
            value: "false".to_string(),
        })
    } else if env::var("AGY_PULSE_FORCE_UNICODE").ok().as_deref() == Some("1") {
        Some(PulseEnvOverride {
            env_var: "AGY_PULSE_FORCE_UNICODE",
            value: "true".to_string(),
        })
    } else {
        None
    };

    PulseEnvOverrides {
        language,
        dashboard_port,
        dashboard_idle_timeout_sec,
        unicode,
    }
}

/// Read the raw global configuration snapshot from `path` without applying environment overrides.
pub fn read_global_config_snapshot_at(path: &Path) -> io::Result<GlobalPulseConfigSnapshot> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(GlobalPulseConfigSnapshot {
                path: path.to_path_buf(),
                exists: false,
                version: ABSENT_CONFIG_VERSION.to_string(),
                raw_object: None,
                parse_error: None,
                config: PulseConfig::default(),
            });
        }
        Err(err) => return Err(err),
    };

    let version = hex::encode(Sha256::digest(&bytes));
    let (raw_object, parse_error, config) = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => {
            let defaults = serde_json::to_value(PulseConfig::default())
                .unwrap_or_else(|_| Value::Object(Map::new()));
            let merged = deep_merge_value(defaults, Value::Object(map.clone()));
            let parsed = serde_json::from_value(merged).unwrap_or_default();
            (Some(map), None, parsed)
        }
        Ok(_) => (
            None,
            Some("global config root must be a JSON object".to_string()),
            PulseConfig::default(),
        ),
        Err(err) => (
            None,
            Some(format!("invalid JSON: {err}")),
            PulseConfig::default(),
        ),
    };

    Ok(GlobalPulseConfigSnapshot {
        path: path.to_path_buf(),
        exists: true,
        version,
        raw_object,
        parse_error,
        config,
    })
}

/// Read the raw global configuration snapshot from `resolve_global_config_path()` without env overrides.
pub fn read_global_config_snapshot() -> io::Result<GlobalPulseConfigSnapshot> {
    let path = resolve_global_config_path();
    read_global_config_snapshot_at(&path)
}

fn is_allowed_theme_color(raw: &str) -> bool {
    let lower = raw.trim().to_ascii_lowercase();
    lower == "grey" || ALLOWED_THEME_COLORS.contains(&lower.as_str())
}

/// Validate a `PulseConfig` payload, returning `Err((field, message))` on the first invalid field.
pub fn validate_pulse_config(cfg: &PulseConfig) -> Result<(), (String, String)> {
    let w = cfg.thresholds.warning;
    if !w.is_finite() || !(0.0..=1.0).contains(&w) {
        return Err((
            "thresholds.warning".to_string(),
            "thresholds.warning must be a finite number between 0.0 and 1.0".to_string(),
        ));
    }
    let c = cfg.thresholds.critical;
    if !c.is_finite() || !(0.0..=1.0).contains(&c) {
        return Err((
            "thresholds.critical".to_string(),
            "thresholds.critical must be a finite number between 0.0 and 1.0".to_string(),
        ));
    }
    if w > c {
        return Err((
            "thresholds.warning".to_string(),
            "thresholds.warning must be <= thresholds.critical".to_string(),
        ));
    }

    match &cfg.theme {
        ThemeConfig::Preset(raw) => {
            let trimmed = raw.trim();
            let is_preset = THEME_PRESETS
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(trimmed));
            if !is_preset && !is_allowed_theme_color(trimmed) {
                return Err((
                    "theme".to_string(),
                    "theme must match a built-in preset name or allowed ANSI color".to_string(),
                ));
            }
        }
        ThemeConfig::Colors(colors) => {
            for (field, val) in [
                ("theme.primary", colors.primary.as_str()),
                ("theme.secondary", colors.secondary.as_str()),
                ("theme.warning", colors.warning.as_str()),
                ("theme.critical", colors.critical.as_str()),
            ] {
                if !is_allowed_theme_color(val) {
                    return Err((
                        field.to_string(),
                        format!("{field} must be one of: green, gray, blue, magenta, yellow, cyan, red"),
                    ));
                }
            }
        }
    }

    if let Some(port) = cfg.display.dashboard_port
        && port == 0
    {
        return Err((
            "display.dashboardPort".to_string(),
            "display.dashboardPort must be between 1 and 65535".to_string(),
        ));
    }

    if let Some(count) = cfg.display.breadcrumb_count
        && count > 20
    {
        return Err((
            "display.breadcrumbCount".to_string(),
            "display.breadcrumbCount must be between 0 and 20".to_string(),
        ));
    }

    if let Some(width) = cfg.display.terminal_width
        && !(20..=1000).contains(&width)
    {
        return Err((
            "display.terminalWidth".to_string(),
            "display.terminalWidth must be between 20 and 1000".to_string(),
        ));
    }

    if let Some(ref username) = cfg.display.username {
        if username.chars().any(|ch| ch.is_control()) {
            return Err((
                "display.username".to_string(),
                "display.username must not contain control characters".to_string(),
            ));
        }
        if username.trim().chars().count() > 64 {
            return Err((
                "display.username".to_string(),
                "display.username must be at most 64 characters".to_string(),
            ));
        }
    }

    if let Some(ref cht) = cfg.display.cache_hit_thresholds {
        if let Some(excellent) = cht.excellent
            && (!excellent.is_finite() || !(0.0..=100.0).contains(&excellent))
        {
            return Err((
                "display.cacheHitThresholds.excellent".to_string(),
                "display.cacheHitThresholds.excellent must be between 0 and 100".to_string(),
            ));
        }
        if let Some(partial) = cht.partial
            && (!partial.is_finite() || !(0.0..=100.0).contains(&partial))
        {
            return Err((
                "display.cacheHitThresholds.partial".to_string(),
                "display.cacheHitThresholds.partial must be between 0 and 100".to_string(),
            ));
        }
        if let (Some(excellent), Some(partial)) = (cht.excellent, cht.partial)
            && partial > excellent
        {
            return Err((
                "display.cacheHitThresholds.partial".to_string(),
                "display.cacheHitThresholds.partial must be <= excellent".to_string(),
            ));
        }
    }

    if let Some(turns) = cfg.display.model_speed_expire_turns
        && turns < 1
    {
        return Err((
            "display.modelSpeedExpireTurns".to_string(),
            "display.modelSpeedExpireTurns must be >= 1".to_string(),
        ));
    }

    if let Some(minutes) = cfg.display.model_speed_expire_minutes
        && (!minutes.is_finite() || minutes <= 0.0)
    {
        return Err((
            "display.modelSpeedExpireMinutes".to_string(),
            "display.modelSpeedExpireMinutes must be a finite number > 0".to_string(),
        ));
    }

    if let Some(ref icons) = cfg.icons {
        for (field, opt_val) in [
            ("icons.branch", icons.branch.as_deref()),
            ("icons.token", icons.token.as_deref()),
            ("icons.ctx", icons.ctx.as_deref()),
        ] {
            if let Some(raw) = opt_val {
                if raw.chars().any(|ch| ch.is_control()) {
                    return Err((
                        field.to_string(),
                        format!("{field} must not contain control characters"),
                    ));
                }
                let trimmed = raw.trim();
                if UnicodeWidthStr::width(trimmed) > 8 {
                    return Err((
                        field.to_string(),
                        format!("{field} display width must be <= 8 columns"),
                    ));
                }
            }
        }
    }

    Ok(())
}

fn set_or_remove_opt_bool(map: &mut Map<String, Value>, key: &str, val: Option<bool>) {
    match val {
        Some(b) => {
            map.insert(key.to_string(), Value::Bool(b));
        }
        None => {
            map.remove(key);
        }
    }
}

fn set_or_remove_opt_u64(map: &mut Map<String, Value>, key: &str, val: Option<u64>) {
    match val {
        Some(n) => {
            map.insert(key.to_string(), Value::from(n));
        }
        None => {
            map.remove(key);
        }
    }
}

fn set_or_remove_opt_f64(map: &mut Map<String, Value>, key: &str, val: Option<f64>) {
    match val {
        Some(n) => {
            map.insert(key.to_string(), Value::from(n));
        }
        None => {
            map.remove(key);
        }
    }
}

fn set_or_remove_opt_trimmed_str(map: &mut Map<String, Value>, key: &str, val: Option<&str>) {
    match val.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            map.insert(key.to_string(), Value::String(s.to_string()));
        }
        None => {
            map.remove(key);
        }
    }
}

/// Save `cfg` atomically to `path` while preserving unknown top-level/nested keys (e.g. `columnWidth`)
/// and removing cleared optional fields. Returns the new file content SHA-256 hex digest.
pub fn save_global_pulse_config_preserving_unknown(
    path: &Path,
    existing_raw: Option<&Map<String, Value>>,
    cfg: &PulseConfig,
) -> io::Result<String> {
    let mut root = existing_raw.cloned().unwrap_or_default();

    root.insert("enabled".to_string(), Value::Bool(cfg.enabled));
    let lang_val = serde_json::to_value(cfg.language)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    root.insert("language".to_string(), lang_val);

    let theme_val = serde_json::to_value(&cfg.theme)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    root.insert("theme".to_string(), theme_val);

    let mut thresholds = match root.remove("thresholds") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    thresholds.insert("warning".to_string(), Value::from(cfg.thresholds.warning));
    thresholds.insert("critical".to_string(), Value::from(cfg.thresholds.critical));
    root.insert("thresholds".to_string(), Value::Object(thresholds));

    let mut display = match root.remove("display") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let d = &cfg.display;
    for (key, val) in [
        ("showContextMonitor", d.show_context_monitor),
        ("showModelCache", d.show_model_cache),
        ("showCurrentDir", d.show_current_dir),
        ("showCycleMode", d.show_cycle_mode),
        ("showSpeed", d.show_speed),
        ("showModel", d.show_model),
        ("showTokenBar", d.show_token_bar),
        ("showContextBar", d.show_context_bar),
        ("showTurnCacheHitRate", d.show_turn_cache_hit_rate),
        ("showBreadcrumbs", d.show_breadcrumbs),
        ("showGitBranch", d.show_git_branch),
        ("showUsername", d.show_username),
        ("showCost", d.show_cost),
        ("showDailyCost", d.show_daily_cost),
        ("showWeeklyCost", d.show_weekly_cost),
        ("showRoutedCost", d.show_routed_cost),
        ("showDashboardUrl", d.show_dashboard_url),
    ] {
        set_or_remove_opt_bool(&mut display, key, val);
    }

    set_or_remove_opt_u64(
        &mut display,
        "dashboardPort",
        d.dashboard_port.map(u64::from),
    );
    set_or_remove_opt_u64(
        &mut display,
        "dashboardIdleTimeoutSec",
        d.dashboard_idle_timeout_sec,
    );
    set_or_remove_opt_u64(
        &mut display,
        "breadcrumbCount",
        d.breadcrumb_count.map(u64::from),
    );
    set_or_remove_opt_bool(&mut display, "useNerdFonts", d.use_nerd_fonts);
    set_or_remove_opt_bool(&mut display, "unicode", d.unicode);

    let quota_style_val = serde_json::to_value(d.quota_style)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    display.insert("quotaStyle".to_string(), quota_style_val);

    set_or_remove_opt_u64(
        &mut display,
        "terminalWidth",
        d.terminal_width.map(u64::from),
    );
    set_or_remove_opt_trimmed_str(&mut display, "username", d.username.as_deref());

    match &d.cache_hit_thresholds {
        Some(cht) if cht.excellent.is_some() || cht.partial.is_some() => {
            let mut cht_map = match display.remove("cacheHitThresholds") {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            set_or_remove_opt_f64(&mut cht_map, "excellent", cht.excellent);
            set_or_remove_opt_f64(&mut cht_map, "partial", cht.partial);
            display.insert("cacheHitThresholds".to_string(), Value::Object(cht_map));
        }
        _ => {
            display.remove("cacheHitThresholds");
        }
    }

    set_or_remove_opt_u64(
        &mut display,
        "modelSpeedExpireTurns",
        d.model_speed_expire_turns,
    );
    set_or_remove_opt_f64(
        &mut display,
        "modelSpeedExpireMinutes",
        d.model_speed_expire_minutes,
    );
    root.insert("display".to_string(), Value::Object(display));

    let mut icons_map = match root.remove("icons") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    set_or_remove_opt_trimmed_str(
        &mut icons_map,
        "branch",
        cfg.icons.as_ref().and_then(|i| i.branch.as_deref()),
    );
    set_or_remove_opt_trimmed_str(
        &mut icons_map,
        "token",
        cfg.icons.as_ref().and_then(|i| i.token.as_deref()),
    );
    set_or_remove_opt_trimmed_str(
        &mut icons_map,
        "ctx",
        cfg.icons.as_ref().and_then(|i| i.ctx.as_deref()),
    );
    if !icons_map.is_empty() {
        root.insert("icons".to_string(), Value::Object(icons_map));
    }

    let mut json_text = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    json_text.push('\n');
    platform::write_atomic_0600(path, json_text.as_bytes())?;
    Ok(hex::encode(Sha256::digest(json_text.as_bytes())))
}
