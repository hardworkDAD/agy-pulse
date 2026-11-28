use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Terminal rendering capabilities probed from OS environment and TTY state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaps {
    pub unicode: bool,
    pub columns: Option<u16>,
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
        match raw
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
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

/// Threshold percentages (0..=100) for statusline cache hit rate coloring.
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
    pub show_breadcrumbs: Option<bool>,
    pub show_git_branch: Option<bool>,
    pub show_username: Option<bool>,
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
            show_breadcrumbs: Some(false),
            show_git_branch: Some(false),
            show_username: Some(false),
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
