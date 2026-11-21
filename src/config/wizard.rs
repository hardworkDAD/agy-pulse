use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::config::{
    LanguageSetting, PulseConfig, QuotaStyle, ThemeColors, ThemeConfig, deep_merge_value,
    load_config, read_json_object,
};
use crate::domain::{
    ChildModelSpeed, ContextWindowSnapshot, CostSnapshot, HostIdentity, ModelIdentity,
    ModelTokenTotals, NamedProviderQuota, ProviderQuota, ProviderQuotaSummary, PulseSnapshot,
    QuotaWindow, TokenUsage, TurnPeakByMode, WorkspaceInfo, WorkspaceMetadata,
};
use crate::platform::{self, RawTerminalGuard, TerminalCaps};
use crate::render::{render_statusline, resolve_language_from_locale};

const CONFIG_FILENAME: &str = "agy-pulse.config.json";
const BREADCRUMB_COUNTS: &[u32] = &[0, 1, 2, 3, 5];
const WARNING_THRESHOLDS: &[f64] = &[0.5, 0.6, 0.7, 0.75];
const CRITICAL_THRESHOLDS: &[f64] = &[0.8, 0.85, 0.9, 0.95];

struct ThemePreset {
    name: &'static str,
    colors: ThemeColorsSpec,
}

#[derive(Clone, Copy)]
struct ThemeColorsSpec {
    primary: &'static str,
    secondary: &'static str,
    warning: &'static str,
    critical: &'static str,
}

impl ThemeColorsSpec {
    fn to_colors(self) -> ThemeColors {
        ThemeColors {
            primary: self.primary.to_string(),
            secondary: self.secondary.to_string(),
            warning: self.warning.to_string(),
            critical: self.critical.to_string(),
        }
    }
}

const THEME_PRESETS: &[ThemePreset] = &[
    ThemePreset {
        name: "Emerald Green",
        colors: ThemeColorsSpec {
            primary: "green",
            secondary: "gray",
            warning: "yellow",
            critical: "red",
        },
    },
    ThemePreset {
        name: "Ocean Blue",
        colors: ThemeColorsSpec {
            primary: "blue",
            secondary: "gray",
            warning: "yellow",
            critical: "red",
        },
    },
    ThemePreset {
        name: "Cyberpunk Magenta",
        colors: ThemeColorsSpec {
            primary: "magenta",
            secondary: "gray",
            warning: "yellow",
            critical: "red",
        },
    },
    ThemePreset {
        name: "Amber Gold",
        colors: ThemeColorsSpec {
            primary: "yellow",
            secondary: "gray",
            warning: "cyan",
            critical: "red",
        },
    },
];

/// Configuration options for running the interactive wizard loop over arbitrary I/O streams.
#[derive(Debug, Clone)]
pub struct WizardRunOptions {
    pub cwd: PathBuf,
    pub terminal_cols: Option<u16>,
    pub terminal_rows: Option<u16>,
    pub unicode: bool,
    pub locale: Option<String>,
    pub observed_at_ms: Option<i64>,
    pub clear_screen: bool,
}

impl Default for WizardRunOptions {
    fn default() -> Self {
        Self {
            cwd: env::current_dir().unwrap_or_default(),
            terminal_cols: Some(120),
            terminal_rows: None,
            unicode: true,
            locale: None,
            observed_at_ms: None,
            clear_screen: true,
        }
    }
}

/// Terminal exit state returned by [`run_wizard_loop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardRunOutcome {
    Saved,
    Cancelled,
    Aborted,
    EndOfInput,
}

#[derive(Debug, Default)]
struct WizardChanges {
    enabled: Option<bool>,
    language: Option<LanguageSetting>,
    theme: Option<ThemeColors>,
    warning_threshold: Option<f64>,
    critical_threshold: Option<f64>,
    display_bools: Map<String, Value>,
    quota_style: Option<QuotaStyle>,
    breadcrumb_count: Option<u32>,
    use_nerd_fonts: Option<bool>,
    /// `Some(None)` removes `display.unicode` when switched back to Auto.
    unicode: Option<Option<bool>>,
}

struct WizardState {
    config: PulseConfig,
    cwd: PathBuf,
    target_is_global: bool,
    theme_name: String,
    original_theme: ThemeColors,
    changes: WizardChanges,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoolDisplayField {
    ContextMonitor,
    ModelCache,
    CurrentDir,
    CycleMode,
    Model,
    GitBranch,
    Username,
    TokenBar,
    ContextBar,
    TurnCacheHitRate,
    Breadcrumbs,
    Speed,
    Cost,
    DailyCost,
    WeeklyCost,
    RoutedCost,
    DashboardUrl,
}

struct BoolFieldMeta {
    field: BoolDisplayField,
    json_key: &'static str,
    label: &'static str,
    default_on: bool,
}

const BOOL_FIELDS: &[BoolFieldMeta] = &[
    BoolFieldMeta {
        field: BoolDisplayField::ContextMonitor,
        json_key: "showContextMonitor",
        label: "Show Context Monitor",
        default_on: true,
    },
    BoolFieldMeta {
        field: BoolDisplayField::ModelCache,
        json_key: "showModelCache",
        label: "Show Model Cache Line",
        default_on: true,
    },
    BoolFieldMeta {
        field: BoolDisplayField::CurrentDir,
        json_key: "showCurrentDir",
        label: "Show Current Directory",
        default_on: true,
    },
    BoolFieldMeta {
        field: BoolDisplayField::CycleMode,
        json_key: "showCycleMode",
        label: "Show Cycle Mode (Shift+Tab)",
        default_on: true,
    },
    BoolFieldMeta {
        field: BoolDisplayField::Model,
        json_key: "showModel",
        label: "Show Model (Line 1)",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::GitBranch,
        json_key: "showGitBranch",
        label: "Show Git Branch",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::Username,
        json_key: "showUsername",
        label: "Show Username",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::TokenBar,
        json_key: "showTokenBar",
        label: "Show Token Bar",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::ContextBar,
        json_key: "showContextBar",
        label: "Show Context Bar (Line 2)",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::TurnCacheHitRate,
        json_key: "showTurnCacheHitRate",
        label: "Show Turn Cache Hit Rate",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::Breadcrumbs,
        json_key: "showBreadcrumbs",
        label: "Show Breadcrumbs",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::Speed,
        json_key: "showSpeed",
        label: "Show Output Speed",
        default_on: true,
    },
    BoolFieldMeta {
        field: BoolDisplayField::Cost,
        json_key: "showCost",
        label: "Show Conversation Cost",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::DailyCost,
        json_key: "showDailyCost",
        label: "Show Daily Cost",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::WeeklyCost,
        json_key: "showWeeklyCost",
        label: "Show Weekly Cost",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::RoutedCost,
        json_key: "showRoutedCost",
        label: "Show Routed Child Cost",
        default_on: false,
    },
    BoolFieldMeta {
        field: BoolDisplayField::DashboardUrl,
        json_key: "showDashboardUrl",
        label: "Show Dashboard URL (Auto-Start)",
        default_on: true,
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Scope,
    Language,
    Theme,
    QuotaStyle,
    WarningThreshold,
    CriticalThreshold,
    IconSet,
    BoolDisplay(BoolDisplayField),
    BreadcrumbCount,
    Enabled,
    Save,
    Cancel,
}

const MENU_ITEMS: &[MenuItem] = &[
    MenuItem::Scope,
    MenuItem::Language,
    MenuItem::Theme,
    MenuItem::QuotaStyle,
    MenuItem::WarningThreshold,
    MenuItem::CriticalThreshold,
    MenuItem::IconSet,
    MenuItem::BoolDisplay(BoolDisplayField::ContextMonitor),
    MenuItem::BoolDisplay(BoolDisplayField::ModelCache),
    MenuItem::BoolDisplay(BoolDisplayField::CurrentDir),
    MenuItem::BoolDisplay(BoolDisplayField::CycleMode),
    MenuItem::BoolDisplay(BoolDisplayField::Model),
    MenuItem::BoolDisplay(BoolDisplayField::GitBranch),
    MenuItem::BoolDisplay(BoolDisplayField::Username),
    MenuItem::BoolDisplay(BoolDisplayField::TokenBar),
    MenuItem::BoolDisplay(BoolDisplayField::ContextBar),
    MenuItem::BoolDisplay(BoolDisplayField::TurnCacheHitRate),
    MenuItem::BoolDisplay(BoolDisplayField::Breadcrumbs),
    MenuItem::BreadcrumbCount,
    MenuItem::BoolDisplay(BoolDisplayField::Speed),
    MenuItem::BoolDisplay(BoolDisplayField::Cost),
    MenuItem::BoolDisplay(BoolDisplayField::DailyCost),
    MenuItem::BoolDisplay(BoolDisplayField::WeeklyCost),
    MenuItem::BoolDisplay(BoolDisplayField::RoutedCost),
    MenuItem::BoolDisplay(BoolDisplayField::DashboardUrl),
    MenuItem::Enabled,
    MenuItem::Save,
    MenuItem::Cancel,
];

#[derive(Debug, Clone, Copy)]
struct ViewportRange {
    start_idx: usize,
    end_idx: usize,
    has_above: bool,
    has_below: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardKey {
    Up,
    Down,
    Toggle,
    Save,
    Cancel,
    Abort,
    Digit(u8),
    Ignore,
}

fn normalize_theme_colors(theme: &ThemeConfig) -> ThemeColors {
    match theme {
        ThemeConfig::Colors(c) => c.clone(),
        ThemeConfig::Preset(raw) => {
            let trimmed = raw.trim();
            if let Some(preset) = THEME_PRESETS
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(trimmed))
            {
                return preset.colors.to_colors();
            }
            let mut base = ThemeColors::default();
            if !trimmed.is_empty() {
                base.primary = trimmed.to_ascii_lowercase();
            }
            base
        }
    }
}

fn detect_theme_preset_name(colors: &ThemeColors) -> String {
    for preset in THEME_PRESETS {
        if colors.primary == preset.colors.primary
            && colors.secondary == preset.colors.secondary
            && colors.warning == preset.colors.warning
            && colors.critical == preset.colors.critical
        {
            return preset.name.to_string();
        }
    }
    "Custom".to_string()
}

fn bool_field_meta(field: BoolDisplayField) -> &'static BoolFieldMeta {
    // `BOOL_FIELDS` covers every `BoolDisplayField` variant.
    BOOL_FIELDS
        .iter()
        .find(|m| m.field == field)
        .unwrap_or(&BOOL_FIELDS[0])
}

fn read_display_bool(config: &PulseConfig, field: BoolDisplayField) -> bool {
    let d = &config.display;
    let opt = match field {
        BoolDisplayField::ContextMonitor => d.show_context_monitor,
        BoolDisplayField::ModelCache => d.show_model_cache,
        BoolDisplayField::CurrentDir => d.show_current_dir,
        BoolDisplayField::CycleMode => d.show_cycle_mode,
        BoolDisplayField::Model => d.show_model,
        BoolDisplayField::GitBranch => d.show_git_branch,
        BoolDisplayField::Username => d.show_username,
        BoolDisplayField::TokenBar => d.show_token_bar,
        BoolDisplayField::ContextBar => d.show_context_bar,
        BoolDisplayField::TurnCacheHitRate => d.show_turn_cache_hit_rate,
        BoolDisplayField::Breadcrumbs => d.show_breadcrumbs,
        BoolDisplayField::Speed => d.show_speed,
        BoolDisplayField::Cost => d.show_cost,
        BoolDisplayField::DailyCost => d.show_daily_cost,
        BoolDisplayField::WeeklyCost => d.show_weekly_cost,
        BoolDisplayField::RoutedCost => d.show_routed_cost,
        BoolDisplayField::DashboardUrl => d.show_dashboard_url,
    };
    opt.unwrap_or_else(|| bool_field_meta(field).default_on)
}

fn write_display_bool(config: &mut PulseConfig, field: BoolDisplayField, val: bool) {
    let d = &mut config.display;
    let slot = match field {
        BoolDisplayField::ContextMonitor => &mut d.show_context_monitor,
        BoolDisplayField::ModelCache => &mut d.show_model_cache,
        BoolDisplayField::CurrentDir => &mut d.show_current_dir,
        BoolDisplayField::CycleMode => &mut d.show_cycle_mode,
        BoolDisplayField::Model => &mut d.show_model,
        BoolDisplayField::GitBranch => &mut d.show_git_branch,
        BoolDisplayField::Username => &mut d.show_username,
        BoolDisplayField::TokenBar => &mut d.show_token_bar,
        BoolDisplayField::ContextBar => &mut d.show_context_bar,
        BoolDisplayField::TurnCacheHitRate => &mut d.show_turn_cache_hit_rate,
        BoolDisplayField::Breadcrumbs => &mut d.show_breadcrumbs,
        BoolDisplayField::Speed => &mut d.show_speed,
        BoolDisplayField::Cost => &mut d.show_cost,
        BoolDisplayField::DailyCost => &mut d.show_daily_cost,
        BoolDisplayField::WeeklyCost => &mut d.show_weekly_cost,
        BoolDisplayField::RoutedCost => &mut d.show_routed_cost,
        BoolDisplayField::DashboardUrl => &mut d.show_dashboard_url,
    };
    *slot = Some(val);
}

fn cycle_f64_preset(current: f64, presets: &[f64]) -> f64 {
    if let Some(idx) = presets.iter().position(|&v| (v - current).abs() < 1e-6) {
        presets[(idx + 1) % presets.len()]
    } else {
        presets
            .iter()
            .copied()
            .find(|&v| v > current + 1e-6)
            .unwrap_or(presets[0])
    }
}

impl WizardState {
    fn new(cwd: PathBuf) -> Self {
        let mut config = load_config(&cwd);
        let original_theme = normalize_theme_colors(&config.theme);
        let theme_name = detect_theme_preset_name(&original_theme);
        config.theme = ThemeConfig::Colors(original_theme.clone());
        let local_exists = cwd.join(CONFIG_FILENAME).exists();

        Self {
            config,
            cwd,
            target_is_global: !local_exists,
            theme_name,
            original_theme,
            changes: WizardChanges::default(),
        }
    }

    fn target_path(&self) -> PathBuf {
        if self.target_is_global {
            platform::resolve_global_config_dir().join(CONFIG_FILENAME)
        } else {
            self.cwd.join(CONFIG_FILENAME)
        }
    }

    fn item_label(&self, item: MenuItem) -> &'static str {
        match item {
            MenuItem::Scope => "Configuration Scope",
            MenuItem::Language => "Language",
            MenuItem::Theme => "Theme Preset",
            MenuItem::QuotaStyle => "Quota Display Mode",
            MenuItem::WarningThreshold => "Warning Threshold",
            MenuItem::CriticalThreshold => "Critical Threshold",
            MenuItem::IconSet => "Icon & Font Set",
            MenuItem::BoolDisplay(field) => bool_field_meta(field).label,
            MenuItem::BreadcrumbCount => "Breadcrumb Limit",
            MenuItem::Enabled => "Enable agy-pulse Status Line",
            MenuItem::Save => "Save & Exit",
            MenuItem::Cancel => "Cancel & Exit",
        }
    }

    fn item_value(&self, item: MenuItem) -> String {
        match item {
            MenuItem::Scope => {
                if self.target_is_global {
                    "Global (All Projects)".to_string()
                } else {
                    "Local (Current Project Workspace)".to_string()
                }
            }
            MenuItem::Language => match self.config.language {
                LanguageSetting::Auto => "auto".to_string(),
                LanguageSetting::En => "en".to_string(),
                LanguageSetting::Zh => "zh".to_string(),
            },
            MenuItem::Theme => self.theme_name.clone(),
            MenuItem::QuotaStyle => match self.config.display.quota_style {
                QuotaStyle::Table => "Table".to_string(),
                QuotaStyle::Compact => "Compact".to_string(),
                QuotaStyle::None => "None".to_string(),
            },
            MenuItem::WarningThreshold => {
                format!("{}%", (self.config.thresholds.warning * 100.0).round() as i64)
            }
            MenuItem::CriticalThreshold => {
                format!("{}%", (self.config.thresholds.critical * 100.0).round() as i64)
            }
            MenuItem::IconSet => {
                if self.config.display.use_nerd_fonts == Some(true) {
                    "Nerd Fonts".to_string()
                } else if self.config.display.unicode == Some(true) {
                    "Unicode Emoji".to_string()
                } else if self.config.display.unicode == Some(false) {
                    "Plain ASCII".to_string()
                } else {
                    "Auto (UTF-8 Detect)".to_string()
                }
            }
            MenuItem::BoolDisplay(field) => {
                if read_display_bool(&self.config, field) {
                    "Enabled".to_string()
                } else {
                    "Disabled".to_string()
                }
            }
            MenuItem::BreadcrumbCount => self.config.display.breadcrumb_count.unwrap_or(3).to_string(),
            MenuItem::Enabled => {
                if self.config.enabled {
                    "Enabled".to_string()
                } else {
                    "Disabled".to_string()
                }
            }
            MenuItem::Save | MenuItem::Cancel => String::new(),
        }
    }

    fn toggle_item(&mut self, item: MenuItem) {
        match item {
            MenuItem::Scope => {
                self.target_is_global = !self.target_is_global;
            }
            MenuItem::Language => {
                let next = match self.config.language {
                    LanguageSetting::Auto => LanguageSetting::En,
                    LanguageSetting::En => LanguageSetting::Zh,
                    LanguageSetting::Zh => LanguageSetting::Auto,
                };
                self.config.language = next;
                self.changes.language = Some(next);
            }
            MenuItem::Theme => {
                let mut names: Vec<&str> = THEME_PRESETS.iter().map(|p| p.name).collect();
                names.push("Custom");
                let cur_idx = names
                    .iter()
                    .position(|&n| n == self.theme_name)
                    .unwrap_or(0);
                let next_name = names[(cur_idx + 1) % names.len()];
                self.theme_name = next_name.to_string();
                let next_colors = THEME_PRESETS
                    .iter()
                    .find(|p| p.name == next_name)
                    .map(|p| p.colors.to_colors())
                    .unwrap_or_else(|| self.original_theme.clone());
                self.config.theme = ThemeConfig::Colors(next_colors.clone());
                self.changes.theme = Some(next_colors);
            }
            MenuItem::QuotaStyle => {
                let next = match self.config.display.quota_style {
                    QuotaStyle::Table => QuotaStyle::Compact,
                    QuotaStyle::Compact => QuotaStyle::None,
                    QuotaStyle::None => QuotaStyle::Table,
                };
                self.config.display.quota_style = next;
                self.changes.quota_style = Some(next);
            }
            MenuItem::WarningThreshold => {
                let next = cycle_f64_preset(self.config.thresholds.warning, WARNING_THRESHOLDS);
                self.config.thresholds.warning = next;
                self.changes.warning_threshold = Some(next);
            }
            MenuItem::CriticalThreshold => {
                let next = cycle_f64_preset(self.config.thresholds.critical, CRITICAL_THRESHOLDS);
                self.config.thresholds.critical = next;
                self.changes.critical_threshold = Some(next);
            }
            MenuItem::IconSet => {
                if self.config.display.use_nerd_fonts == Some(true) {
                    self.config.display.use_nerd_fonts = Some(false);
                    self.config.display.unicode = None;
                    self.changes.use_nerd_fonts = Some(false);
                    self.changes.unicode = Some(None);
                } else if self.config.display.unicode.is_none() {
                    self.config.display.use_nerd_fonts = Some(false);
                    self.config.display.unicode = Some(true);
                    self.changes.use_nerd_fonts = Some(false);
                    self.changes.unicode = Some(Some(true));
                } else if self.config.display.unicode == Some(true) {
                    self.config.display.use_nerd_fonts = Some(false);
                    self.config.display.unicode = Some(false);
                    self.changes.use_nerd_fonts = Some(false);
                    self.changes.unicode = Some(Some(false));
                } else {
                    self.config.display.use_nerd_fonts = Some(true);
                    self.config.display.unicode = Some(true);
                    self.changes.use_nerd_fonts = Some(true);
                    self.changes.unicode = Some(Some(true));
                }
            }
            MenuItem::BoolDisplay(field) => {
                let next = !read_display_bool(&self.config, field);
                write_display_bool(&mut self.config, field, next);
                self.changes
                    .display_bools
                    .insert(bool_field_meta(field).json_key.to_string(), Value::Bool(next));
            }
            MenuItem::BreadcrumbCount => {
                let cur = self.config.display.breadcrumb_count.unwrap_or(3);
                let next = if let Some(idx) = BREADCRUMB_COUNTS.iter().position(|&v| v == cur) {
                    BREADCRUMB_COUNTS[(idx + 1) % BREADCRUMB_COUNTS.len()]
                } else {
                    BREADCRUMB_COUNTS
                        .iter()
                        .copied()
                        .find(|&v| v > cur)
                        .unwrap_or(BREADCRUMB_COUNTS[0])
                };
                self.config.display.breadcrumb_count = Some(next);
                self.changes.breadcrumb_count = Some(next);
            }
            MenuItem::Enabled => {
                self.config.enabled = !self.config.enabled;
                self.changes.enabled = Some(self.config.enabled);
            }
            MenuItem::Save | MenuItem::Cancel => {}
        }
    }
}

fn compute_menu_viewport(
    total_items: usize,
    selected_idx: usize,
    term_rows: Option<u16>,
    preview_line_count: usize,
) -> ViewportRange {
    let Some(rows) = term_rows.filter(|&r| r > 0).map(usize::from) else {
        return ViewportRange {
            start_idx: 0,
            end_idx: total_items,
            has_above: false,
            has_below: false,
        };
    };
    let reserved_lines = 8 + preview_line_count.max(1);
    let max_visible = rows.saturating_sub(reserved_lines).max(3);
    if total_items <= max_visible {
        return ViewportRange {
            start_idx: 0,
            end_idx: total_items,
            has_above: false,
            has_below: false,
        };
    }
    let window_size = max_visible.saturating_sub(2).max(3);
    let max_start = total_items.saturating_sub(window_size);
    let start_idx = selected_idx.saturating_sub(window_size / 2).min(max_start);
    let end_idx = (start_idx + window_size).min(total_items);
    ViewportRange {
        start_idx,
        end_idx,
        has_above: start_idx > 0,
        has_below: end_idx < total_items,
    }
}

fn build_sample_pulse_snapshot(observed_at_ms: i64) -> PulseSnapshot {
    let five_hour_reset = Some(observed_at_ms + 4 * 3_600_000);
    let weekly_reset = Some(observed_at_ms + 5 * 86_400_000);
    let sample_usage = TokenUsage {
        input_tokens: 196_600,
        output_tokens: 1_200,
        cache_read_input_tokens: 160_000,
        cache_creation_input_tokens: 0,
        cache_exclusive: true,
    };

    PulseSnapshot {
        has_conversation_identity: true,
        model: Some(ModelIdentity {
            id: "gemini-4-carbon-high".to_string(),
            display_name: "Gemini 4 Carbon (High)".to_string(),
            effort: None,
        }),
        host: HostIdentity {
            conversation_id: Some("wizard-preview".to_string()),
            session_id: None,
            plan_tier: Some("Pro".to_string()),
            cycle_mode: Some("auto".to_string()),
            username: Some("developer@agy.local".to_string()),
        },
        workspace: WorkspaceInfo {
            current_dir: "agy-pulse".to_string(),
            git_branch: Some("main".to_string()),
        },
        workspace_meta: WorkspaceMetadata {
            memory_file: None,
            breadcrumbs: vec![
                "index.rs".to_string(),
                "config.rs".to_string(),
                "wizard.rs".to_string(),
            ],
            rules_count: 4,
            mcp_count: 2,
            hooks_count: 3,
        },
        context: Some(ContextWindowSnapshot {
            context_window_size: 1_000_000,
            used_percentage: Some(38.4),
            total_input_tokens: 196_600,
            total_output_tokens: 13_900,
            total_cache_read_tokens: Some(160_000),
            total_cache_creation_tokens: Some(0),
            current_usage: Some(sample_usage.clone()),
            has_total_input: true,
        }),
        cost: CostSnapshot {
            native_total_usd: Some(0.085),
            cascade_usd: Some(0.125),
            children_cost_usd: Some(0.04),
            today_usd: Some(0.45),
            week_usd: Some(1.82),
        },
        terminal_width: None,
        task_count: 5,
        transcript_steps: 12,
        compaction_count: 1,
        server_compaction_limit: Some(512_000),
        peak_raw_turn_tokens: 480_000,
        turn_history_max_by_mode: TurnPeakByMode {
            inclusive_peak: 480_000,
            exclusive_peak: 480_000,
        },
        last_turn_usage: Some(sample_usage),
        last_turn_model: Some("Gemini 4 Carbon (High)".to_string()),
        max_historical_cache: 160_000,
        turn_cache_metrics: None,
        transcript_total_input_tokens: 196_600,
        transcript_total_output_tokens: 13_900,
        output_speed_tok_s: Some(43.4),
        main_model_last_active_ms: Some(observed_at_ms),
        main_model_turns_ago: Some(0),
        child_model_speeds: vec![ChildModelSpeed {
            model: "Hydro".to_string(),
            speed_tok_s: 64.2,
            ttft_sec: Some(4.3),
            turns_ago: 0,
            last_active_ms: observed_at_ms,
        }],
        model_token_totals: vec![
            ModelTokenTotals {
                model_key: "Carbon".to_string(),
                input_tokens: 38_800_000,
                cache_read_tokens: 33_400_000,
                cache_write_tokens: 0,
                output_tokens: 120_000,
                steps: 89,
            },
            ModelTokenTotals {
                model_key: "Hydro".to_string(),
                input_tokens: 6_400_000,
                cache_read_tokens: 4_800_000,
                cache_write_tokens: 0,
                output_tokens: 42_000,
                steps: 24,
            },
        ],
        image_exhausted: None,
        ttft_sec: Some(2.4),
        update_available_version: None,
        dashboard_url: Some("http://127.0.0.1:3789".to_string()),
        observed_at_ms,
        quota: Some(ProviderQuotaSummary {
            google: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.92,
                    reset_time_ms: five_hour_reset,
                }),
                weekly: Some(QuotaWindow {
                    remaining_fraction: 0.55,
                    reset_time_ms: weekly_reset,
                }),
            },
            claude: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.70,
                    reset_time_ms: five_hour_reset,
                }),
                weekly: Some(QuotaWindow {
                    remaining_fraction: 0.25,
                    reset_time_ms: weekly_reset,
                }),
            },
            argon: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.85,
                    reset_time_ms: five_hour_reset,
                }),
                weekly: None,
            },
            extras: Vec::<NamedProviderQuota>::new(),
        }),
    }
}

fn render_wizard_preview(
    state: &WizardState,
    snapshot: &PulseSnapshot,
    options: &WizardRunOptions,
) -> String {
    if !state.config.enabled {
        return "\x1b[90m(agy-pulse status line is disabled)\x1b[0m".to_string();
    }
    let preview_term = TerminalCaps {
        unicode: state.config.display.unicode.unwrap_or(options.unicode),
        columns: state
            .config
            .display
            .terminal_width
            .or(options.terminal_cols),
    };
    let lang = resolve_language_from_locale(state.config.language, options.locale.as_deref());
    let rendered = render_statusline(snapshot, &state.config, &preview_term, lang);
    if rendered.is_empty() {
        "\x1b[90m(no visible statusline elements selected)\x1b[0m".to_string()
    } else {
        rendered
    }
}

fn render_wizard_frame<W: Write>(
    output: &mut W,
    state: &WizardState,
    selected_idx: usize,
    snapshot: &PulseSnapshot,
    options: &WizardRunOptions,
) -> io::Result<()> {
    if options.clear_screen {
        write!(output, "\x1b[H\x1b[J")?;
    }

    let preview_body = render_wizard_preview(state, snapshot, options);
    let preview_line_count = preview_body.lines().count().max(1);
    let viewport = compute_menu_viewport(
        MENU_ITEMS.len(),
        selected_idx,
        options.terminal_rows,
        preview_line_count,
    );

    writeln!(
        output,
        "\x1b[1m\x1b[35m=== agy-pulse Configuration Wizard ===\x1b[0m\n"
    )?;
    writeln!(
        output,
        "Use \x1b[1m↑/↓\x1b[0m, \x1b[1mj/k\x1b[0m, or \x1b[1m1-9/0\x1b[0m to navigate, \x1b[1mSpace/Enter\x1b[0m to toggle, \x1b[1ms\x1b[0m to save, \x1b[1mq\x1b[0m to cancel.\n"
    )?;

    if viewport.has_above {
        writeln!(
            output,
            "  \x1b[90m▲ ({} more above)\x1b[0m",
            viewport.start_idx
        )?;
    }

    for (idx, &item) in MENU_ITEMS
        .iter()
        .enumerate()
        .take(viewport.end_idx)
        .skip(viewport.start_idx)
    {
        let is_selected = idx == selected_idx;
        let marker = if is_selected {
            "\x1b[32m▸ \x1b[0m"
        } else {
            "  "
        };
        match item {
            MenuItem::Save => {
                let action = if is_selected {
                    "\x1b[32m[ Save (s) ]\x1b[0m"
                } else {
                    "[ Save (s) ]"
                };
                writeln!(output, "{marker}{action}")?;
            }
            MenuItem::Cancel => {
                let action = if is_selected {
                    "\x1b[31m[ Cancel (q) ]\x1b[0m"
                } else {
                    "[ Cancel (q) ]"
                };
                writeln!(output, "{marker}{action}")?;
            }
            _ => {
                let label = state.item_label(item);
                let val = state.item_value(item);
                let label_str = if is_selected {
                    format!("\x1b[1m{label}\x1b[0m")
                } else {
                    label.to_string()
                };
                writeln!(output, "{marker}{label_str}: \x1b[36m{val}\x1b[0m")?;
            }
        }
    }

    if viewport.has_below {
        writeln!(
            output,
            "  \x1b[90m▼ ({} more below)\x1b[0m",
            MENU_ITEMS.len().saturating_sub(viewport.end_idx)
        )?;
    }

    let separator = "\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m";
    writeln!(output, "\n{separator}")?;
    writeln!(output, "\x1b[1magy-pulse Status Line Preview\x1b[0m")?;
    writeln!(output, "{preview_body}")?;
    writeln!(output, "{separator}")?;
    output.flush()
}

fn build_override_value(changes: &WizardChanges) -> Value {
    let mut root = Map::new();

    if let Some(enabled) = changes.enabled {
        root.insert("enabled".to_string(), Value::Bool(enabled));
    }
    if let Some(lang) = changes.language
        && let Ok(val) = serde_json::to_value(lang)
    {
        root.insert("language".to_string(), val);
    }
    if let Some(ref theme) = changes.theme
        && let Ok(val) = serde_json::to_value(theme)
    {
        root.insert("theme".to_string(), val);
    }

    let mut thresholds = Map::new();
    if let Some(w) = changes.warning_threshold {
        thresholds.insert("warning".to_string(), Value::from(w));
    }
    if let Some(c) = changes.critical_threshold {
        thresholds.insert("critical".to_string(), Value::from(c));
    }
    if !thresholds.is_empty() {
        root.insert("thresholds".to_string(), Value::Object(thresholds));
    }

    let mut display = changes.display_bools.clone();
    if let Some(qs) = changes.quota_style
        && let Ok(val) = serde_json::to_value(qs)
    {
        display.insert("quotaStyle".to_string(), val);
    }
    if let Some(bc) = changes.breadcrumb_count {
        display.insert("breadcrumbCount".to_string(), Value::from(bc));
    }
    if let Some(nf) = changes.use_nerd_fonts {
        display.insert("useNerdFonts".to_string(), Value::Bool(nf));
    }
    if let Some(Some(u)) = changes.unicode {
        display.insert("unicode".to_string(), Value::Bool(u));
    }
    if !display.is_empty() {
        root.insert("display".to_string(), Value::Object(display));
    }

    Value::Object(root)
}

fn save_wizard_config(state: &WizardState) -> io::Result<PathBuf> {
    let target_path = state.target_path();
    let existing = read_json_object(&target_path).unwrap_or_else(|| Value::Object(Map::new()));
    let override_val = build_override_value(&state.changes);
    let mut merged = deep_merge_value(existing, override_val);

    if matches!(state.changes.unicode, Some(None))
        && let Some(display_map) = merged.get_mut("display").and_then(Value::as_object_mut)
    {
        display_map.remove("unicode");
    }

    let mut json_text = serde_json::to_string_pretty(&merged)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    json_text.push('\n');
    platform::write_atomic_0600(&target_path, json_text.as_bytes())?;
    Ok(target_path)
}

fn read_single_byte<R: Read>(input: &mut R) -> io::Result<Option<u8>> {
    let mut buf = [0u8; 1];
    match input.read(&mut buf) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(buf[0])),
        Err(err) => Err(err),
    }
}

fn consume_csi_sequence<R: Read>(input: &mut R) -> io::Result<WizardKey> {
    let Some(third) = read_single_byte(input)? else {
        return Ok(WizardKey::Ignore);
    };
    match third {
        b'A' => Ok(WizardKey::Up),
        b'B' => Ok(WizardKey::Down),
        b'0'..=b'9' => {
            // Drain remaining CSI parameter/final bytes (e.g. `\x1b[5~`) so they don't leak into menu commands.
            while let Some(b) = read_single_byte(input)? {
                if (0x40..=0x7e).contains(&b) {
                    break;
                }
            }
            Ok(WizardKey::Ignore)
        }
        _ => Ok(WizardKey::Ignore),
    }
}

fn read_next_key<R: Read>(input: &mut R) -> io::Result<Option<WizardKey>> {
    let Some(first) = read_single_byte(input)? else {
        return Ok(None);
    };

    let key = match first {
        0x03 => WizardKey::Abort,
        b'q' | b'Q' => WizardKey::Cancel,
        b's' | b'S' => WizardKey::Save,
        b'k' => WizardKey::Up,
        b'j' => WizardKey::Down,
        b' ' | b'\r' | b'\n' => WizardKey::Toggle,
        b'0'..=b'9' => WizardKey::Digit(first - b'0'),
        0x1b => match read_single_byte(input)? {
            None => WizardKey::Cancel,
            Some(b'[') => consume_csi_sequence(input)?,
            Some(b'O') => match read_single_byte(input)? {
                Some(b'A') => WizardKey::Up,
                Some(b'B') => WizardKey::Down,
                _ => WizardKey::Ignore,
            },
            Some(_) => WizardKey::Cancel,
        },
        _ => WizardKey::Ignore,
    };
    Ok(Some(key))
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(1_700_000_000_000)
}

/// Execute the interactive configuration wizard over caller-provided `Read` and `Write` streams.
pub fn run_wizard_loop<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    options: &WizardRunOptions,
) -> io::Result<WizardRunOutcome> {
    let mut state = WizardState::new(options.cwd.clone());
    let observed_at_ms = options.observed_at_ms.unwrap_or_else(current_epoch_ms);
    let snapshot = build_sample_pulse_snapshot(observed_at_ms);
    let mut selected_idx: usize = 0;

    render_wizard_frame(output, &state, selected_idx, &snapshot, options)?;

    loop {
        let Some(key) = read_next_key(input)? else {
            return Ok(WizardRunOutcome::EndOfInput);
        };

        let mut should_render = false;
        match key {
            WizardKey::Abort => {
                writeln!(output, "\n\x1b[90mConfiguration canceled.\x1b[0m\n")?;
                output.flush()?;
                return Ok(WizardRunOutcome::Aborted);
            }
            WizardKey::Cancel => {
                writeln!(output, "\n\x1b[90mConfiguration canceled.\x1b[0m\n")?;
                output.flush()?;
                return Ok(WizardRunOutcome::Cancelled);
            }
            WizardKey::Save => {
                let saved_path = save_wizard_config(&state)?;
                writeln!(
                    output,
                    "\n\x1b[32m✔ Configuration saved successfully to {}\x1b[0m\n",
                    saved_path.display()
                )?;
                output.flush()?;
                return Ok(WizardRunOutcome::Saved);
            }
            WizardKey::Up => {
                selected_idx = (selected_idx + MENU_ITEMS.len() - 1) % MENU_ITEMS.len();
                should_render = true;
            }
            WizardKey::Down => {
                selected_idx = (selected_idx + 1) % MENU_ITEMS.len();
                should_render = true;
            }
            WizardKey::Digit(num) => {
                let target_idx = if num == 0 { 9 } else { usize::from(num - 1) };
                if target_idx < MENU_ITEMS.len() {
                    selected_idx = target_idx;
                    should_render = true;
                }
            }
            WizardKey::Toggle => {
                let item = MENU_ITEMS[selected_idx];
                match item {
                    MenuItem::Save => {
                        let saved_path = save_wizard_config(&state)?;
                        writeln!(
                            output,
                            "\n\x1b[32m✔ Configuration saved successfully to {}\x1b[0m\n",
                            saved_path.display()
                        )?;
                        output.flush()?;
                        return Ok(WizardRunOutcome::Saved);
                    }
                    MenuItem::Cancel => {
                        writeln!(output, "\n\x1b[90mConfiguration canceled.\x1b[0m\n")?;
                        output.flush()?;
                        return Ok(WizardRunOutcome::Cancelled);
                    }
                    _ => {
                        state.toggle_item(item);
                        should_render = true;
                    }
                }
            }
            WizardKey::Ignore => {}
        }

        if should_render {
            render_wizard_frame(output, &state, selected_idx, &snapshot, options)?;
        }
    }
}

fn default_target_config_path(cwd: &Path) -> PathBuf {
    let local = cwd.join(CONFIG_FILENAME);
    if local.exists() {
        local
    } else {
        platform::resolve_global_config_dir().join(CONFIG_FILENAME)
    }
}

/// Entrypoint for `agypl config` / `agy-pulse --config` CLI subcommand.
pub fn run_config_cli() -> i32 {
    let cwd = env::current_dir().unwrap_or_default();
    if !platform::is_stdin_stdout_tty() {
        let target = default_target_config_path(&cwd);
        eprintln!(
            "agy-pulse config requires an interactive TTY terminal. Edit {} directly or run inside an interactive terminal.",
            target.display()
        );
        return 0;
    }

    let _raw_guard = match RawTerminalGuard::enter() {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("Failed to enter raw terminal mode: {err}");
            return 1;
        }
    };

    let term_caps = platform::detect_terminal_caps(None, None, None);
    let term_rows = platform::detect_live_tty_rows();
    let options = WizardRunOptions {
        cwd,
        terminal_cols: term_caps.columns.or(Some(120)),
        terminal_rows: term_rows,
        unicode: term_caps.unicode,
        locale: platform::read_locale_env(),
        observed_at_ms: None,
        clear_screen: true,
    };

    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    match run_wizard_loop(&mut stdin, &mut stdout, &options) {
        Ok(WizardRunOutcome::Saved | WizardRunOutcome::Cancelled | WizardRunOutcome::EndOfInput) => {
            0
        }
        Ok(WizardRunOutcome::Aborted) => 1,
        Err(err) => {
            eprintln!("\n\x1b[31mConfiguration wizard error: {err}\x1b[0m\n");
            1
        }
    }
}
