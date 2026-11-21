use crate::config::{LanguageSetting, PulseConfig};
use crate::domain::{PulseSnapshot, format_usd, is_usage_cache_inclusive, resolve_cascade_cost_usd};
use crate::platform::TerminalCaps;

use super::context_bar::{
    Glyphs, ProgressBarStyle, create_progress_bar, render_context_monitor_line,
    render_model_cache_line,
};
use super::i18n::{BOLD, RESET, get_language_text, resolve_palette};
use super::width::{
    ansi_display_width, format_tokens, sanitize_terminal_text, simplify_model_name,
    truncate_ansi_to_display_width,
};

fn format_cycle_mode_segment(cycle_mode: Option<&str>) -> String {
    let Some(raw) = cycle_mode else {
        return String::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "auto" => format!("{BOLD}\x1b[33mauto{RESET}"),
        "accept-edits" => format!("{BOLD}\x1b[32maccept-edits{RESET}"),
        "plan" => format!("{BOLD}\x1b[94mplan{RESET}"),
        "bestn" => format!("{BOLD}\x1b[35mbest-of-n{RESET}"),
        _ => {
            let sanitized = sanitize_terminal_text(trimmed, 24);
            let clean = sanitized.trim();
            if clean.is_empty() {
                String::new()
            } else {
                format!("{BOLD}\x1b[36m{clean}{RESET}")
            }
        }
    }
}

fn resolve_line1_model_name(snapshot: &PulseSnapshot) -> String {
    let Some(model) = &snapshot.model else {
        return "Unknown Model".to_string();
    };
    let mut base = if !model.display_name.is_empty() {
        model.display_name.clone()
    } else if !model.id.is_empty() {
        model.id.clone()
    } else {
        return "Unknown Model".to_string();
    };
    if let Some(effort) = &model.effort {
        let trimmed = effort.trim();
        if !trimmed.is_empty()
            && !base
                .to_ascii_lowercase()
                .contains(&trimmed.to_ascii_lowercase())
        {
            base = format!("{base} ({trimmed})");
        }
    }
    sanitize_terminal_text(&simplify_model_name(&base), 80)
}

/// Pure function rendering the multi-line statusline output from a `PulseSnapshot`,
/// `PulseConfig`, `TerminalCaps`, and pre-resolved `LanguageSetting`.
pub fn render_statusline(
    snapshot: &PulseSnapshot,
    config: &PulseConfig,
    term: &TerminalCaps,
    resolved_lang: LanguageSetting,
) -> String {
    if !config.enabled {
        return String::new();
    }

    let d = &config.display;
    let use_nerd = d.use_nerd_fonts.unwrap_or(false);
    let unicode = term.unicode;
    let show_git_branch = d.show_git_branch.unwrap_or(true);
    let show_token_bar = d.show_token_bar.unwrap_or(true);
    let show_current_dir = d.show_current_dir.unwrap_or(true);
    let show_username = d.show_username.unwrap_or(false);
    let show_cost = d.show_cost.unwrap_or(false);
    let show_daily_cost = d.show_daily_cost.unwrap_or(false);
    let show_weekly_cost = d.show_weekly_cost.unwrap_or(false);
    let show_speed = d.show_speed.unwrap_or(true);
    let show_model = d.show_model.unwrap_or(true);
    let show_context_bar = d.show_context_bar.unwrap_or(true);
    let show_cycle_mode = d.show_cycle_mode.unwrap_or(true);
    let allow_routed_cost = d.show_routed_cost.unwrap_or(false);

    let text = get_language_text(resolved_lang);
    let palette = resolve_palette(&config.theme);

    let warn_thresh = config.thresholds.warning;
    let crit_thresh = config.thresholds.critical;

    let explicit_cols = term.columns.filter(|&c| c > 0).map(|c| c as usize);

    let glyph = if unicode {
        Glyphs {
            bar: "█",
            empty: "░",
            vbar: "│",
            ellipsis: "…",
        }
    } else {
        Glyphs {
            bar: "#",
            empty: "-",
            vbar: "|",
            ellipsis: "...",
        }
    };

    let (mut branch_icon, mut token_icon, mut ctx_icon) = if use_nerd {
        ("\u{e725} ".to_string(), "\u{f06a9} ".to_string(), "\u{f1510} ".to_string())
    } else if unicode {
        ("⎇ ".to_string(), "⚿ ".to_string(), "⛁ ".to_string())
    } else {
        ("[B] ".to_string(), "[Tk] ".to_string(), "[C] ".to_string())
    };

    if let Some(icons) = &config.icons {
        if let Some(v) = &icons.branch {
            branch_icon = sanitize_terminal_text(v, 8);
        }
        if let Some(v) = &icons.token {
            token_icon = sanitize_terminal_text(v, 8);
        }
        if let Some(v) = &icons.ctx {
            ctx_icon = sanitize_terminal_text(v, 8);
        }
    }

    let divider = format!(" {}{}{RESET} ", palette.secondary, glyph.vbar);

    // Line 1 segments
    let current_dir = sanitize_terminal_text(&snapshot.workspace.current_dir, 80);
    let raw_username = d
        .username
        .as_deref()
        .or(snapshot.host.username.as_deref())
        .unwrap_or("");
    let username = sanitize_terminal_text(raw_username, 80);
    let raw_branch = sanitize_terminal_text(
        snapshot.workspace.git_branch.as_deref().unwrap_or(""),
        80,
    );

    let username_seg = if show_username && !username.is_empty() {
        format!("{}{username}{RESET}", palette.cyan)
    } else {
        String::new()
    };
    let current_dir_seg = if show_current_dir && !current_dir.is_empty() {
        format!("{}{current_dir}{RESET}", palette.blue)
    } else {
        String::new()
    };
    let branch_seg = if show_git_branch && !raw_branch.is_empty() {
        format!("{}{branch_icon}{raw_branch}{RESET}", palette.blue)
    } else {
        String::new()
    };
    let model_seg = if show_model {
        let model_name = resolve_line1_model_name(snapshot);
        format!("{}{model_name}{RESET}", palette.primary)
    } else {
        String::new()
    };
    let plan = sanitize_terminal_text(
        snapshot.host.plan_tier.as_deref().unwrap_or("Free"),
        80,
    );
    let plan_seg = format!("{}{plan}{RESET}", palette.magenta);
    let mode_seg = if show_cycle_mode {
        format_cycle_mode_segment(snapshot.host.cycle_mode.as_deref())
    } else {
        String::new()
    };

    let cascade_cost = if show_cost {
        resolve_cascade_cost_usd(snapshot, allow_routed_cost)
    } else {
        None
    };

    let mut cost_parts: Vec<String> = Vec::new();
    if let Some(sc) = cascade_cost {
        cost_parts.push(format!("{} {}", text.cost_label, format_usd(sc)));
    }
    if show_daily_cost && let Some(today) = snapshot.cost.today_usd {
        cost_parts.push(format!("{} {}", text.today_label, format_usd(today)));
    }
    if show_weekly_cost && let Some(week) = snapshot.cost.week_usd {
        cost_parts.push(format!("{} {}", text.week_label, format_usd(week)));
    }
    let cost_seg = if cost_parts.is_empty() {
        String::new()
    } else {
        format!("{}{}{RESET}", palette.secondary, cost_parts.join(" | "))
    };

    let speed_seg = if show_speed && let Some(speed) = snapshot.output_speed_tok_s {
        format!(
            "{}{}: {speed:.1} {}{RESET}",
            palette.secondary, text.out_label, text.tok_per_sec
        )
    } else {
        String::new()
    };

    let update_seg = match &snapshot.update_available_version {
        Some(ver) if !ver.trim().is_empty() => {
            let icon = if unicode { "⟳" } else { "[UP]" };
            format!("{}{icon} v{}{RESET}", palette.warning, ver.trim())
        }
        _ => String::new(),
    };

    let assemble_line1 = |inc_update: bool, inc_branch: bool, inc_dir: bool, inc_user: bool| {
        let mut parts: Vec<&str> = Vec::new();
        if inc_user && !username_seg.is_empty() {
            parts.push(&username_seg);
        }
        if inc_dir && !current_dir_seg.is_empty() {
            parts.push(&current_dir_seg);
        }
        if inc_branch && !branch_seg.is_empty() {
            parts.push(&branch_seg);
        }
        if !model_seg.is_empty() {
            parts.push(&model_seg);
        }
        parts.push(&plan_seg);
        if !mode_seg.is_empty() {
            parts.push(&mode_seg);
        }
        if !cost_seg.is_empty() {
            parts.push(&cost_seg);
        }
        if !speed_seg.is_empty() {
            parts.push(&speed_seg);
        }
        if inc_update && !update_seg.is_empty() {
            parts.push(&update_seg);
        }
        parts.join(&divider)
    };

    let mut line1 = assemble_line1(true, true, true, true);
    if let Some(cols) = explicit_cols
        && ansi_display_width(&line1) > cols
    {
        let steps = [
            (false, true, true, true),
            (false, false, true, true),
            (false, false, false, true),
            (false, false, false, false),
        ];
        for (u, b, dir, usr) in steps {
            line1 = assemble_line1(u, b, dir, usr);
            if ansi_display_width(&line1) <= cols {
                break;
            }
        }
        if ansi_display_width(&line1) > cols {
            line1 = truncate_ansi_to_display_width(&line1, cols, glyph.ellipsis);
        }
    }

    // Line 2 (Tokens + Context bar)
    let cw = snapshot.context.as_ref();
    let total_input = cw.map(|c| c.total_input_tokens).unwrap_or(0);
    let total_output = cw.map(|c| c.total_output_tokens).unwrap_or(0);
    let ctx_pct_raw = cw.and_then(|c| c.used_percentage).unwrap_or(0.0);
    let cur = cw.and_then(|c| c.current_usage.as_ref());
    let cache_read = cur
        .map(|u| u.cache_read_input_tokens)
        .or_else(|| cw.and_then(|c| c.total_cache_read_tokens))
        .unwrap_or(0);
    let cache_write = cur
        .map(|u| u.cache_creation_input_tokens)
        .or_else(|| cw.and_then(|c| c.total_cache_creation_tokens))
        .unwrap_or(0);
    let cache_total = cache_read.saturating_add(cache_write);

    let raw_model_name = snapshot
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

    let in_tokens = match cur {
        Some(u) => {
            if is_usage_cache_inclusive(raw_model_name, u) {
                u.input_tokens.saturating_sub(cache_total)
            } else {
                u.input_tokens
            }
        }
        None => total_input.saturating_sub(cache_total),
    };
    let token_total = in_tokens
        .saturating_add(total_output)
        .saturating_add(cache_total);

    let mut detail_parts = vec![
        format!("in: {}", format_tokens(in_tokens)),
        format!("out: {}", format_tokens(total_output)),
    ];
    if cache_total > 0 {
        detail_parts.push(format!("cache: {}", format_tokens(cache_total)));
    }
    let token_prefix = if token_icon == "[Tk] " {
        "Tokens".to_string()
    } else {
        format!("{token_icon}Tokens")
    };
    let tokens_str = format!(
        "{}{token_prefix} {} {}({}{RESET}{}{}){RESET}",
        palette.cyan,
        format_tokens(token_total),
        palette.secondary,
        RESET,
        detail_parts.join(", "),
        palette.secondary,
    );

    let effective_ctx_size = cw
        .map(|c| c.context_window_size)
        .filter(|&s| s > 0)
        .or(snapshot.server_compaction_limit)
        .unwrap_or(0);

    let (display_ctx_tokens, effective_ctx_percent) = if ctx_pct_raw > 0.0 || cur.is_none() {
        (total_input, ctx_pct_raw)
    } else if let Some(u) = cur {
        let active = in_tokens
            .saturating_add(cache_total)
            .saturating_add(u.output_tokens);
        let pct = if effective_ctx_size > 0 {
            ((active as f64 * 100.0) / effective_ctx_size as f64).min(100.0)
        } else {
            0.0
        };
        (active, pct)
    } else {
        (total_input, ctx_pct_raw)
    };

    let ctx_bar = create_progress_bar(
        effective_ctx_percent,
        &ProgressBarStyle {
            default_color: palette.cyan,
            width: 10,
            is_usage: true,
            warn_thresh,
            crit_thresh,
            palette: &palette,
            glyph: &glyph,
        },
    );
    let ctx_pct_str = format!("{}%", effective_ctx_percent.round() as u64);
    let ctx_str = format!(
        "{}{ctx_icon}{}/{}{RESET} {ctx_bar} {}{ctx_pct_str}{RESET}",
        palette.cyan,
        format_tokens(display_ctx_tokens),
        format_tokens(effective_ctx_size),
        palette.cyan,
    );

    let mut line2_parts: Vec<String> = Vec::new();
    if show_token_bar {
        line2_parts.push(tokens_str);
    }
    if show_context_bar {
        line2_parts.push(ctx_str);
    }
    let mut line2 = line2_parts.join(&divider);
    if let Some(cols) = explicit_cols
        && !line2.is_empty()
        && ansi_display_width(&line2) > cols
    {
        line2 = truncate_ansi_to_display_width(&line2, cols, glyph.ellipsis);
    }

    let mut lines: Vec<String> = Vec::new();
    if !line1.is_empty() {
        lines.push(line1);
    }
    if !line2.is_empty() {
        lines.push(line2);
    }

    let show_context_monitor = d.show_context_monitor.unwrap_or(false);
    let show_model_cache =
        d.show_model_cache == Some(true) || (d.show_model_cache != Some(false) && show_context_monitor);

    if show_model_cache
        && let Some(cache_line) = render_model_cache_line(snapshot, term, text, &palette)
        && !cache_line.is_empty()
    {
        lines.push(cache_line);
    }

    if show_context_monitor {
        let monitor_line =
            render_context_monitor_line(snapshot, config, term, text, &palette);
        if !monitor_line.is_empty() {
            lines.push(monitor_line);
        }
    }

    lines.join("\n")
}
