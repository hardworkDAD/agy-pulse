mod context_bar;
mod i18n;
mod layout;
mod quota_table;
mod width;

pub use layout::render_statusline;
pub use width::{
    ansi_display_width, display_width, strip_ansi, truncate_ansi_to_display_width,
};

use crate::config::LanguageSetting;

/// Resolve `LanguageSetting::Auto` using an explicit locale string (pure, no env access).
pub fn resolve_language_from_locale(setting: LanguageSetting, locale: Option<&str>) -> LanguageSetting {
    match setting {
        LanguageSetting::En => LanguageSetting::En,
        LanguageSetting::Zh => LanguageSetting::Zh,
        LanguageSetting::Auto => {
            if let Some(loc) = locale {
                let lower = loc.trim().to_ascii_lowercase();
                if lower == "zh" || lower.starts_with("zh_") || lower.starts_with("zh-") {
                    return LanguageSetting::Zh;
                }
            }
            LanguageSetting::En
        }
    }
}
