use crate::config::PulseConfig;
use crate::domain::{
    PulseSnapshot, is_usage_cache_inclusive, resolve_cascade_cost_usd,
};
use crate::platform::TerminalCaps;

use super::i18n::{BOLD, DIM, LanguageText, RESET, ThemePalette};
use super::width::{
    ansi_display_width, compact_model_name, format_monitor_tokens, sanitize_terminal_text,
    truncate_ansi_to_display_width,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct Glyphs {
    pub bar: &'static str,
    pub empty: &'static str,
    pub vbar: &'static str,
    pub ellipsis: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ProgressBarStyle<'a> {
    pub default_color: &'a str,
    pub width: usize,
    pub is_usage: bool,
    pub warn_thresh: f64,
    pub crit_thresh: f64,
    pub palette: &'a ThemePalette,
    pub glyph: &'a Glyphs,
}

pub(super) fn create_progress_bar(percent: f64, style: &ProgressBarStyle<'_>) -> String {
    let safe_pct = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let completed =
        ((safe_pct / 100.0) * style.width as f64).round().clamp(0.0, style.width as f64) as usize;
    let remaining = style.width.saturating_sub(completed);

    let final_color = if style.is_usage {
        if safe_pct > style.crit_thresh * 100.0 {
            style.palette.critical
        } else if safe_pct > style.warn_thresh * 100.0 {
            style.palette.warning
        } else {
            style.default_color
        }
    } else if safe_pct <= (1.0 - style.crit_thresh) * 100.0 {
        style.palette.critical
    } else if safe_pct <= (1.0 - style.warn_thresh) * 100.0 {
        style.palette.warning
    } else {
        style.palette.primary
    };

    format!(
        "{final_color}[{}{}]{RESET}",
        style.glyph.bar.repeat(completed),
        style.glyph.empty.repeat(remaining)
    )
}

/// Build a 5-level smooth progress bar (`█▓▒░·` in UTF-8, `#=-.` in ASCII).
pub(super) fn build_smooth_bar(pct: f64, width: usize, unicode: bool) -> String {
    let clamped = if pct.is_finite() {
        pct.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let exact = (clamped * width as f64) / 100.0;
    let filled = exact.floor() as usize;
    let rem = ((exact - filled as f64) * 100.0).round() as i64;

    let (full_ch, empty_ch, high_ch, mid_ch, low_ch) = if unicode {
        ('█', '·', '▓', '▒', '░')
    } else {
        ('#', '.', '=', '=', '-')
    };

    let mut out = String::with_capacity(width * 3);
    for i in 0..width {
        if i < filled {
            out.push(full_ch);
        } else if i == filled {
            if rem >= 75 {
                out.push(high_ch);
            } else if rem >= 50 {
                out.push(mid_ch);
            } else if rem >= 25 {
                out.push(low_ch);
            } else {
                out.push(empty_ch);
            }
        } else {
            out.push(empty_ch);
        }
    }
    out
}

pub(super) fn resolve_monitor_model_label(snapshot: &PulseSnapshot) -> String {
    let Some(model) = &snapshot.model else {
        return "Unknown Model".to_string();
    };
    let mut name = if !model.display_name.is_empty() {
        model.display_name.clone()
    } else {
        model.id.clone()
    };
    if name.is_empty() {
        return "Unknown Model".to_string();
    }
    if let Some(effort) = &model.effort {
        let trimmed = effort.trim();
        if !trimmed.is_empty()
            && !name
                .to_ascii_lowercase()
                .contains(&trimmed.to_ascii_lowercase())
        {
            name = format!("{name} ({trimmed})");
        }
    }
    sanitize_terminal_text(&name, 80)
}

#[derive(Clone, Copy)]
enum ModelDisplayMode {
    Full,
    Compact,
    None,
}

struct MonitorLineOpts {
    bar_width: usize,
    compact_sep: bool,
    show_cache_abs: bool,
    show_compaction_peak: bool,
    show_out: bool,
    model_display_mode: ModelDisplayMode,
    show_cache: bool,
    show_compaction: bool,
}

pub(super) fn render_context_monitor_line(
    snapshot: &PulseSnapshot,
    config: &PulseConfig,
    term: &TerminalCaps,
    text: &LanguageText,
    palette: &ThemePalette,
) -> String {
    let has_explicit_cols = term.columns.is_some_and(|c| c > 0);
    let cols = term.columns.filter(|&c| c > 0).unwrap_or(120) as usize;
    let unicode = term.unicode;

    let model_name = resolve_monitor_model_label(snapshot);
    let cw = snapshot.context.as_ref();
    let raw_win_size = cw.map(|c| c.context_window_size).unwrap_or(0);
    let raw_used_pct = cw.and_then(|c| c.used_percentage).unwrap_or(0.0);

    let cur = cw.and_then(|c| c.current_usage.as_ref()).filter(|u| {
        u.input_tokens > 0
            || u.cache_creation_input_tokens > 0
            || u.cache_read_input_tokens > 0
            || u.output_tokens > 0
    });

    let has_turn = cur.is_some();
    let inp = cur.map(|u| u.input_tokens).unwrap_or(0);
    let c_create = cur.map(|u| u.cache_creation_input_tokens).unwrap_or(0);
    let c_read = cur.map(|u| u.cache_read_input_tokens).unwrap_or(0);
    let turn_out = cur.map(|u| u.output_tokens).unwrap_or(0);

    let raw_model_ident = snapshot
        .model
        .as_ref()
        .map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.as_str()
            } else {
                m.id.as_str()
            }
        })
        .unwrap_or("");

    let cache_total = c_create.saturating_add(c_read);
    let use_inclusive_mode = cur.is_some_and(|u| is_usage_cache_inclusive(raw_model_ident, u));
    let prompt_ctx = if use_inclusive_mode {
        inp
    } else {
        inp.saturating_add(cache_total)
    };
    let mut active_ctx = prompt_ctx.saturating_add(turn_out);

    if active_ctx == 0 && raw_used_pct > 0.0 && raw_win_size > 0 {
        active_ctx = ((raw_win_size as f64 * raw_used_pct) / 100.0).round() as u64;
    }

    let ckpts = snapshot.compaction_count;
    let peak_ctx = snapshot.peak_raw_turn_tokens.max(active_ctx);
    let server_limit = snapshot.server_compaction_limit.unwrap_or(0);
    let effective_limit = match (raw_win_size > 0, server_limit > 0) {
        (true, true) => raw_win_size.min(server_limit),
        (false, true) => server_limit,
        (true, false) => raw_win_size,
        (false, false) => 512_000,
    };

    let real_pct = if active_ctx > 0 && effective_limit > 0 {
        (active_ctx as f64 * 100.0) / effective_limit as f64
    } else {
        0.0
    };

    let warn_pct = if (config.thresholds.warning - 0.7).abs() > f64::EPSILON
        && config.thresholds.warning.is_finite()
    {
        config.thresholds.warning * 100.0
    } else {
        65.0
    };
    let crit_pct = if (config.thresholds.critical - 0.9).abs() > f64::EPSILON
        && config.thresholds.critical.is_finite()
    {
        config.thresholds.critical * 100.0
    } else {
        85.0
    };

    let bar_color = if real_pct >= crit_pct {
        palette.critical
    } else if real_pct >= warn_pct {
        palette.warning
    } else {
        palette.cyan
    };

    let sep = if unicode {
        format!("{DIM} · {RESET}")
    } else {
        format!("{DIM} | {RESET}")
    };
    let compact_unicode_sep = format!("{DIM}·{RESET}");
    let model_glyph = if unicode { "◈" } else { "*" };
    let times_glyph = if unicode { "×" } else { "x" };
    let tot_out = cw.map(|c| c.total_output_tokens).unwrap_or(0);
    let allow_routed = config.display.show_routed_cost.unwrap_or(false);
    let raw_usd = resolve_cascade_cost_usd(snapshot, allow_routed);

    let build_line = |opts: &MonitorLineOpts| -> String {
        let active_sep = if unicode && opts.compact_sep {
            &compact_unicode_sep
        } else {
            &sep
        };
        let bar = build_smooth_bar(real_pct, opts.bar_width, unicode);
        let ctx_seg = if active_ctx > 0 || has_turn {
            format!(
                "{DIM}{}{RESET} {bar_color}{bar}{RESET} {BOLD}{:.1}%{RESET} {DIM}({}/{}){RESET}",
                text.ctx_label,
                real_pct,
                format_monitor_tokens(active_ctx),
                format_monitor_tokens(effective_limit),
            )
        } else {
            format!(
                "{DIM}{}{RESET} {DIM}{bar}{RESET} {BOLD}0.0%{RESET} {DIM}({} {}){RESET}",
                text.ctx_label,
                text.limit_label,
                format_monitor_tokens(effective_limit),
            )
        };

        let mut parts: Vec<String> = Vec::new();
        if cols >= 80 && !model_name.is_empty() {
            match opts.model_display_mode {
                ModelDisplayMode::Full => {
                    parts.push(format!("{}{} {model_name}{RESET}", palette.magenta, model_glyph));
                }
                ModelDisplayMode::Compact => {
                    let short = compact_model_name(&model_name);
                    let label = if short.is_empty() { &model_name } else { &short };
                    parts.push(format!("{}{} {label}{RESET}", palette.magenta, model_glyph));
                }
                ModelDisplayMode::None => {}
            }
        }

        parts.push(ctx_seg);

        if ckpts > 0 && opts.show_compaction {
            if opts.show_compaction_peak && cols >= 95 && peak_ctx > 0 {
                let peak_prefix = if text.is_zh {
                    text.peak_label.to_string()
                } else {
                    format!("{} ", text.peak_label)
                };
                parts.push(format!(
                    "{}{}{times_glyph}{ckpts}{RESET}{DIM}({peak_prefix}{}){RESET}",
                    palette.warning,
                    text.compacted_label,
                    format_monitor_tokens(peak_ctx),
                ));
            } else {
                parts.push(format!(
                    "{}{}{times_glyph}{ckpts}{RESET}",
                    palette.warning, text.compacted_label
                ));
            }
        }

        if prompt_ctx > 0 && opts.show_cache {
            let cache_pct = ((c_read as f64 * 100.0) / prompt_ctx as f64).clamp(0.0, 100.0);
            let cache_color = if cache_pct >= 50.0 {
                palette.primary
            } else {
                DIM
            };
            if opts.show_cache_abs && cols >= 105 && c_read > 0 {
                parts.push(format!(
                    "{DIM}{}{RESET} {cache_color}{:.0}%{RESET} {DIM}({}){RESET}",
                    text.cache_label_monitor,
                    cache_pct,
                    format_monitor_tokens(c_read),
                ));
            } else {
                parts.push(format!(
                    "{DIM}{}{RESET} {cache_color}{:.0}%{RESET}",
                    text.cache_label_monitor, cache_pct
                ));
            }
        }

        if opts.show_out && cols >= 85 && tot_out > 0 {
            parts.push(format!(
                "{DIM}{}{RESET} {BOLD}{}{RESET}",
                text.out_label_monitor,
                format_monitor_tokens(tot_out)
            ));
        }

        if cols >= 95
            && let Some(usd) = raw_usd
            && usd.is_finite()
            && usd > 0.0
        {
            let usd_fmt = if usd < 0.01 {
                format!("${usd:.3}")
            } else {
                format!("${usd:.2}")
            };
            parts.push(format!(
                "{DIM}{}{RESET} {BOLD}{usd_fmt}{RESET}",
                text.cost_label_monitor
            ));
        }

        if real_pct >= crit_pct {
            parts.push(format!(
                "{}{BOLD}{}{RESET}",
                palette.critical, text.compaction_imminent
            ));
        }

        parts.join(active_sep)
    };

    let mut opts = MonitorLineOpts {
        bar_width: if cols >= 80 { 12 } else { 8 },
        compact_sep: false,
        show_cache_abs: true,
        show_compaction_peak: true,
        show_out: true,
        model_display_mode: ModelDisplayMode::Full,
        show_cache: true,
        show_compaction: true,
    };

    let mut joined = build_line(&opts);
    if !has_explicit_cols || ansi_display_width(&joined) <= cols {
        return joined;
    }

    for step in 0..11 {
        match step {
            0 if opts.bar_width > 8 => opts.bar_width = 8,
            1 if opts.bar_width > 6 => opts.bar_width = 6,
            2 if opts.bar_width > 4 => opts.bar_width = 4,
            3 if unicode => opts.compact_sep = true,
            4 => opts.show_cache_abs = false,
            5 => opts.show_compaction_peak = false,
            6 => opts.show_out = false,
            7 => opts.model_display_mode = ModelDisplayMode::Compact,
            8 => opts.model_display_mode = ModelDisplayMode::None,
            9 => opts.show_cache = false,
            10 => opts.show_compaction = false,
            _ => continue,
        }
        joined = build_line(&opts);
        if ansi_display_width(&joined) <= cols {
            return joined;
        }
    }

    truncate_ansi_to_display_width(&joined, cols, if unicode { "…" } else { "..." })
}

pub(super) fn render_model_cache_line(
    snapshot: &PulseSnapshot,
    term: &TerminalCaps,
    text: &LanguageText,
    palette: &ThemePalette,
) -> Option<String> {
    let cw = snapshot.context.as_ref()?;
    let cur = cw.current_usage.as_ref().filter(|u| {
        u.input_tokens > 0
            || u.cache_creation_input_tokens > 0
            || u.cache_read_input_tokens > 0
            || u.output_tokens > 0
    })?;

    let cache_total = cur
        .cache_creation_input_tokens
        .saturating_add(cur.cache_read_input_tokens);
    let ident = snapshot
        .model
        .as_ref()
        .map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.as_str()
            } else {
                m.id.as_str()
            }
        })
        .unwrap_or("");
    let use_inclusive = is_usage_cache_inclusive(ident, cur);
    let uncached_in = if use_inclusive {
        cur.input_tokens.saturating_sub(cache_total)
    } else {
        cur.input_tokens
    };
    let total_prompt = uncached_in
        .saturating_add(cur.cache_read_input_tokens)
        .saturating_add(cur.cache_creation_input_tokens);
    if total_prompt == 0 {
        return None;
    }

    let model_key = {
        let short = compact_model_name(ident);
        if short.is_empty() {
            "Model".to_string()
        } else {
            short
        }
    };

    let unicode = term.unicode;
    let has_explicit_cols = term.columns.is_some_and(|c| c > 0);
    let cols = term.columns.filter(|&c| c > 0).unwrap_or(120) as usize;

    let icon = if unicode { "⟳" } else { "~" };
    let prefix = format!("{}{icon}{RESET} {DIM}{}{RESET}", palette.cyan, text.model_cache_label);
    let normal_sep = if unicode {
        format!("{DIM} · {RESET}")
    } else {
        format!("{DIM} | {RESET}")
    };
    let compact_sep = if unicode {
        format!("{DIM}·{RESET}")
    } else {
        format!("{DIM}|{RESET}")
    };
    let inner_dot = if unicode { " · " } else { ", " };

    let hit_pct = ((cur.cache_read_input_tokens as f64 * 100.0) / total_prompt as f64).clamp(0.0, 100.0);
    let hit_color = if hit_pct >= 80.0 {
        format!("{}{BOLD}", palette.primary)
    } else if hit_pct >= 50.0 {
        palette.warning.to_string()
    } else {
        DIM.to_string()
    };
    let head = format!("{model_key} {hit_color}{hit_pct:.0}%{RESET}");
    let hit_fmt = format_monitor_tokens(cur.cache_read_input_tokens);
    let total_fmt = format_monitor_tokens(total_prompt);
    let write_fmt = if cur.cache_creation_input_tokens > 0 {
        Some(format_monitor_tokens(cur.cache_creation_input_tokens))
    } else {
        None
    };

    let full_detail = match &write_fmt {
        Some(w) => format!(
            "{} {hit_fmt}{inner_dot}{} {w} / {} {total_fmt}",
            text.cache_hit_label, text.cache_write_label, text.cache_in_label
        ),
        None => format!(
            "{} {hit_fmt} / {} {total_fmt}",
            text.cache_hit_label, text.cache_in_label
        ),
    };
    let slash_detail = match &write_fmt {
        Some(w) => format!("{hit_fmt}+{w}/{total_fmt}"),
        None => format!("{hit_fmt}/{total_fmt}"),
    };

    let candidates = [
        format!("{prefix}{normal_sep}{head} {DIM}({full_detail}){RESET}"),
        format!("{prefix}{normal_sep}{head} {DIM}({slash_detail}){RESET}"),
        format!("{prefix}{normal_sep}{head} {DIM}({hit_fmt}){RESET}"),
        format!("{prefix}{compact_sep}{head} {DIM}({hit_fmt}){RESET}"),
        format!("{prefix}{compact_sep}{head}"),
    ];

    if !has_explicit_cols {
        return Some(candidates[0].clone());
    }
    for line in &candidates {
        if ansi_display_width(line) <= cols {
            return Some(line.clone());
        }
    }
    Some(truncate_ansi_to_display_width(
        &candidates[4],
        cols,
        if unicode { "…" } else { "..." },
    ))
}
