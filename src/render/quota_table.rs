use crate::domain::{
    ProviderQuota, ProviderQuotaSummary, PulseConfig, QuotaStyle, QuotaWindow, TerminalCaps,
    format_quota_percent, format_reset_countdown, has_valid_provider_quota, is_openai_provider_key,
    pick_critical_window, prune_expired_provider_quota,
};

use super::context_bar::{Glyphs, ProgressBarStyle, create_progress_bar};
use super::i18n::{RESET, ThemePalette};
use super::width::{
    ansi_display_width, display_width, sanitize_terminal_text, truncate_and_pad,
    truncate_ansi_to_display_width,
};

fn quota_pct_color(pct: u8, warn_thresh: f64, crit_thresh: f64, palette: &ThemePalette) -> &str {
    let pct_f = pct as f64;
    let crit_limit = 100.0 - crit_thresh * 100.0;
    let warn_limit = 100.0 - warn_thresh * 100.0;
    if pct_f <= crit_limit {
        palette.critical
    } else if pct_f <= warn_limit {
        palette.warning
    } else {
        palette.primary
    }
}

fn format_provider_display_label(raw_key: &str) -> String {
    let lower = raw_key.trim().to_ascii_lowercase();
    match lower.as_str() {
        "google" => "Google".to_string(),
        "claude" => "Claude".to_string(),
        "argon" => "Argon".to_string(),
        _ => {
            let cleaned = sanitize_terminal_text(raw_key, 16);
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                return "Other".to_string();
            }
            let mut chars = trimmed.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => "Other".to_string(),
            }
        }
    }
}

fn format_weekly_window_label(provider_label: &str) -> String {
    let full = format!("{provider_label} week");
    if display_width(&full) > 11 {
        format!("{provider_label} 7d")
    } else {
        full
    }
}

fn render_window_item(
    label: &str,
    win: Option<&QuotaWindow>,
    observed_at_ms: i64,
    warn_thresh: f64,
    crit_thresh: f64,
    palette: &ThemePalette,
    glyph: &Glyphs,
) -> String {
    let name_part = truncate_and_pad(label, 11);
    let colored_name = format!("{}{name_part}{RESET}", palette.cyan);

    let Some(w) = win else {
        let empty_bar = format!("{}[{}]{RESET}", palette.secondary, glyph.empty.repeat(6));
        let missing_pct = format!("{}  --{RESET}", palette.secondary);
        let missing_time = format!("{}      {RESET}", palette.secondary);
        return format!("{colored_name} {empty_bar} {missing_pct} {missing_time}");
    };

    let pct = format_quota_percent(w.remaining_fraction);
    let pct_color = quota_pct_color(pct, warn_thresh, crit_thresh, palette);
    let bar_part = create_progress_bar(
        pct as f64,
        &ProgressBarStyle {
            default_color: palette.primary,
            width: 6,
            is_usage: false,
            warn_thresh,
            crit_thresh,
            palette,
            glyph,
        },
    );
    let pct_str = format!("{pct:>3}%");
    let colored_pct = format!("{pct_color}{pct_str}{RESET}");

    let raw_time = format_reset_countdown(w.reset_time_ms, observed_at_ms).unwrap_or_default();
    let time_part = format!("{raw_time:<6}");
    let colored_time = format!("{}{time_part}{RESET}", palette.secondary);

    format!("{colored_name} {bar_part} {colored_pct} {colored_time}")
}

fn render_provider_quota_table(
    summary: &ProviderQuotaSummary,
    observed_at_ms: i64,
    config: &PulseConfig,
    term: &TerminalCaps,
    palette: &ThemePalette,
    glyph: &Glyphs,
) -> String {
    let warn_thresh = config.thresholds.warning;
    let crit_thresh = config.thresholds.critical;
    let explicit_cols = term.columns.filter(|&c| c > 0).map(|c| c as usize);

    let google = prune_expired_provider_quota(&summary.google, observed_at_ms);
    let claude = prune_expired_provider_quota(&summary.claude, observed_at_ms);
    let argon = prune_expired_provider_quota(&summary.argon, observed_at_ms);

    let g5 = render_window_item(
        "Google 5h",
        google.five_hour.as_ref(),
        observed_at_ms,
        warn_thresh,
        crit_thresh,
        palette,
        glyph,
    );
    let gw = render_window_item(
        "Google week",
        google.weekly.as_ref(),
        observed_at_ms,
        warn_thresh,
        crit_thresh,
        palette,
        glyph,
    );
    let c5 = render_window_item(
        "Claude 5h",
        claude.five_hour.as_ref(),
        observed_at_ms,
        warn_thresh,
        crit_thresh,
        palette,
        glyph,
    );
    let cw = render_window_item(
        "Claude week",
        claude.weekly.as_ref(),
        observed_at_ms,
        warn_thresh,
        crit_thresh,
        palette,
        glyph,
    );

    let mut rows = vec![
        format!("  {g5} {}{}{RESET} {c5}", palette.secondary, glyph.vbar),
        format!("  {gw} {}{}{RESET} {cw}", palette.secondary, glyph.vbar),
    ];

    let mut extra_providers: Vec<(String, ProviderQuota)> = vec![("Argon".to_string(), argon)];
    for extra in &summary.extras {
        if is_openai_provider_key(&extra.key) {
            continue;
        }
        let label = format_provider_display_label(&extra.key);
        if label.eq_ignore_ascii_case("Google")
            || label.eq_ignore_ascii_case("Claude")
            || label.eq_ignore_ascii_case("Argon")
            || is_openai_provider_key(&label)
        {
            continue;
        }
        extra_providers.push((
            label,
            prune_expired_provider_quota(&extra.quota, observed_at_ms),
        ));
    }

    for (label, pq) in extra_providers {
        match (pq.five_hour.as_ref(), pq.weekly.as_ref()) {
            (Some(five), Some(week)) => {
                let w5 = render_window_item(
                    &format!("{label} 5h"),
                    Some(five),
                    observed_at_ms,
                    warn_thresh,
                    crit_thresh,
                    palette,
                    glyph,
                );
                let ww = render_window_item(
                    &format_weekly_window_label(&label),
                    Some(week),
                    observed_at_ms,
                    warn_thresh,
                    crit_thresh,
                    palette,
                    glyph,
                );
                rows.push(format!(
                    "  {w5} {}{}{RESET} {ww}",
                    palette.secondary, glyph.vbar
                ));
            }
            (None, Some(week)) => {
                let ww = render_window_item(
                    &format_weekly_window_label(&label),
                    Some(week),
                    observed_at_ms,
                    warn_thresh,
                    crit_thresh,
                    palette,
                    glyph,
                );
                rows.push(format!("  {ww}"));
            }
            (Some(five), None) => {
                let w5 = render_window_item(
                    &format!("{label} 5h"),
                    Some(five),
                    observed_at_ms,
                    warn_thresh,
                    crit_thresh,
                    palette,
                    glyph,
                );
                rows.push(format!("  {w5}"));
            }
            (None, None) => {}
        }
    }

    let plain_row_width = ansi_display_width(&rows[0]).saturating_sub(2);
    let base_divider_width = plain_row_width.max(67);
    let (final_rows, divider_indent, divider_width) = match explicit_cols {
        Some(cols) if cols < 2 + base_divider_width => {
            let clipped = rows
                .into_iter()
                .map(|r| {
                    if ansi_display_width(&r) > cols {
                        truncate_ansi_to_display_width(&r, cols, glyph.ellipsis)
                    } else {
                        r
                    }
                })
                .collect();
            if cols <= 2 {
                (clipped, "", cols)
            } else {
                let div_w = base_divider_width.min(cols - 2);
                (clipped, "  ", div_w)
            }
        }
        _ => (rows, "  ", base_divider_width),
    };

    let divider_line = format!(
        "{divider_indent}{}{}{RESET}",
        palette.secondary,
        glyph.hbar.repeat(divider_width)
    );
    format!("{divider_line}\n{}", final_rows.join("\n"))
}

fn render_compact_quota_line(
    summary: &ProviderQuotaSummary,
    observed_at_ms: i64,
    config: &PulseConfig,
    term: &TerminalCaps,
    palette: &ThemePalette,
    glyph: &Glyphs,
    divider: &str,
) -> Option<String> {
    let warn_thresh = config.thresholds.warning;
    let crit_thresh = config.thresholds.critical;

    let mut providers: Vec<(String, &ProviderQuota)> = vec![
        ("Google".to_string(), &summary.google),
        ("Claude".to_string(), &summary.claude),
        ("Argon".to_string(), &summary.argon),
    ];
    for extra in &summary.extras {
        if is_openai_provider_key(&extra.key) {
            continue;
        }
        let label = format_provider_display_label(&extra.key);
        if label.eq_ignore_ascii_case("Google")
            || label.eq_ignore_ascii_case("Claude")
            || label.eq_ignore_ascii_case("Argon")
            || is_openai_provider_key(&label)
        {
            continue;
        }
        providers.push((label, &extra.quota));
    }

    let mut segments: Vec<String> = Vec::new();
    for (label, pq) in providers {
        let Some(critical) = pick_critical_window(pq, observed_at_ms) else {
            continue;
        };
        let pct = format_quota_percent(critical.window.remaining_fraction);
        let pct_color = quota_pct_color(pct, warn_thresh, crit_thresh, palette);
        segments.push(format!(
            "{}{label}:{RESET} {pct_color}{pct}%{RESET}",
            palette.secondary
        ));
    }

    if segments.is_empty() {
        return None;
    }

    let mut line = segments.join(divider);
    if let Some(cols) = term.columns.filter(|&c| c > 0).map(|c| c as usize)
        && ansi_display_width(&line) > cols
    {
        line = truncate_ansi_to_display_width(&line, cols, glyph.ellipsis);
    }
    Some(line)
}

/// Pure function rendering the quota table or compact summary line according to `QuotaStyle`.
pub(super) fn render_quota_section(
    snapshot: &crate::domain::PulseSnapshot,
    config: &PulseConfig,
    term: &TerminalCaps,
    palette: &ThemePalette,
    glyph: &Glyphs,
    divider: &str,
) -> Option<String> {
    if config.display.quota_style == QuotaStyle::None {
        return None;
    }
    let summary = snapshot.quota.as_ref()?;
    if !has_valid_provider_quota(summary, snapshot.observed_at_ms) {
        return None;
    }

    match config.display.quota_style {
        QuotaStyle::None => None,
        QuotaStyle::Compact => render_compact_quota_line(
            summary,
            snapshot.observed_at_ms,
            config,
            term,
            palette,
            glyph,
            divider,
        ),
        QuotaStyle::Table => Some(render_provider_quota_table(
            summary,
            snapshot.observed_at_ms,
            config,
            term,
            palette,
            glyph,
        )),
    }
}
