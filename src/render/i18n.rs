use crate::config::{LanguageSetting, ThemeConfig};

pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const DIM: &str = "\x1b[2m";

pub(super) fn ansi_color(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "gray" | "grey" => Some("\x1b[90m"),
        "blue" => Some("\x1b[34m"),
        "magenta" => Some("\x1b[35m"),
        "yellow" => Some("\x1b[33m"),
        "cyan" => Some("\x1b[36m"),
        "green" => Some("\x1b[32m"),
        "red" => Some("\x1b[31m"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ThemePalette {
    pub primary: &'static str,
    pub secondary: &'static str,
    pub warning: &'static str,
    pub critical: &'static str,
    pub blue: &'static str,
    pub magenta: &'static str,
    pub cyan: &'static str,
}

pub(super) fn resolve_palette(theme: &ThemeConfig) -> ThemePalette {
    let (p_name, s_name, w_name, c_name) = match theme {
        ThemeConfig::Preset(raw) => {
            let trimmed = raw.trim();
            match trimmed.to_ascii_lowercase().as_str() {
                "emerald green" => ("green", "gray", "yellow", "red"),
                "ocean blue" => ("blue", "gray", "yellow", "red"),
                "cyberpunk magenta" => ("magenta", "gray", "yellow", "red"),
                "amber gold" => ("yellow", "gray", "cyan", "red"),
                other => {
                    if ansi_color(other).is_some() {
                        (trimmed, "gray", "yellow", "red")
                    } else {
                        ("green", "gray", "yellow", "red")
                    }
                }
            }
        }
        ThemeConfig::Colors(colors) => (
            colors.primary.as_str(),
            colors.secondary.as_str(),
            colors.warning.as_str(),
            colors.critical.as_str(),
        ),
    };

    ThemePalette {
        primary: ansi_color(p_name).unwrap_or("\x1b[32m"),
        secondary: ansi_color(s_name).unwrap_or("\x1b[90m"),
        warning: ansi_color(w_name).unwrap_or("\x1b[33m"),
        critical: ansi_color(c_name).unwrap_or("\x1b[31m"),
        blue: "\x1b[34m",
        magenta: "\x1b[35m",
        cyan: "\x1b[36m",
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LanguageText {
    pub is_zh: bool,
    pub cost_label: &'static str,
    pub today_label: &'static str,
    pub week_label: &'static str,
    pub out_label: &'static str,
    pub tok_per_sec: &'static str,
    pub ctx_label: &'static str,
    pub limit_label: &'static str,
    pub compacted_label: &'static str,
    pub peak_label: &'static str,
    pub cache_label_monitor: &'static str,
    pub model_cache_label: &'static str,
    pub cache_hit_label: &'static str,
    pub cache_write_label: &'static str,
    pub cache_in_label: &'static str,
    pub out_label_monitor: &'static str,
    pub cost_label_monitor: &'static str,
    pub compaction_imminent: &'static str,
}

const EN_TEXT: LanguageText = LanguageText {
    is_zh: false,
    cost_label: "Cost",
    today_label: "Today",
    week_label: "Week",
    out_label: "out",
    tok_per_sec: "tok/s",
    ctx_label: "Ctx",
    limit_label: "limit",
    compacted_label: "Compacted",
    peak_label: "peak",
    cache_label_monitor: "Cache",
    model_cache_label: "Session Cache",
    cache_hit_label: "hit",
    cache_write_label: "write",
    cache_in_label: "in",
    out_label_monitor: "Out",
    cost_label_monitor: "Cost",
    compaction_imminent: "Compaction imminent",
};

const ZH_TEXT: LanguageText = LanguageText {
    is_zh: true,
    cost_label: "费用",
    today_label: "今日",
    week_label: "本周",
    out_label: "输出",
    tok_per_sec: "tok/s",
    ctx_label: "上下文",
    limit_label: "上限",
    compacted_label: "压缩",
    peak_label: "峰",
    cache_label_monitor: "缓存",
    model_cache_label: "累计缓存",
    cache_hit_label: "命中",
    cache_write_label: "写入",
    cache_in_label: "总入",
    out_label_monitor: "输出",
    cost_label_monitor: "费用",
    compaction_imminent: "即将触发压缩",
};

pub(super) fn get_language_text(lang: LanguageSetting) -> &'static LanguageText {
    match lang {
        LanguageSetting::Zh => &ZH_TEXT,
        LanguageSetting::En | LanguageSetting::Auto => &EN_TEXT,
    }
}
