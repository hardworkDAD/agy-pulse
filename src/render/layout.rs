use crate::domain::{
    ChildModelSpeed, LanguageSetting, PulseConfig, PulseSnapshot, TerminalCaps,
    compact_model_name, format_usd,
};

use super::context_bar::{
    Glyphs, render_context_monitor_line, render_model_cache_line,
};
use super::i18n::{BOLD, LanguageText, RESET, ThemePalette, get_language_text, resolve_palette};
use super::quota_table::render_quota_section;
use super::width::{
    ansi_display_width, sanitize_terminal_text, truncate_ansi_to_display_width,
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

fn format_ttft(ttft_sec: f64, is_zh: bool) -> Option<String> {
    if !ttft_sec.is_finite() || ttft_sec < 0.0 {
        return None;
    }
    let val_str = if ttft_sec < 1.0 {
        format!("{}ms", (ttft_sec * 1000.0).round() as u64)
    } else {
        format!("{ttft_sec:.1}s")
    };
    Some(if is_zh {
        format!("首字 {val_str}")
    } else {
        format!("ttft {val_str}")
    })
}

fn format_child_model_speeds_seg(
    speeds: &[&ChildModelSpeed],
    unicode: bool,
    use_nerd: bool,
    include_ttft: bool,
    text: &LanguageText,
) -> String {
    if speeds.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::with_capacity(speeds.len());
    for item in speeds {
        if !item.speed_tok_s.is_finite() || item.speed_tok_s <= 0.0 || item.speed_tok_s > 5000.0 {
            continue;
        }
        let cleaned = sanitize_terminal_text(item.model.trim(), 24);
        let label = if cleaned.is_empty() {
            "Sub".to_string()
        } else {
            cleaned
        };
        let ttft_suffix = if include_ttft
            && let Some(ttft) = item.ttft_sec
            && let Some(s) = format_ttft(ttft, text.is_zh)
        {
            format!(" ({s})")
        } else {
            String::new()
        };
        parts.push(format!(
            "{label}: {:.1} {}{ttft_suffix}",
            item.speed_tok_s, text.tok_per_sec
        ));
    }
    if parts.is_empty() {
        return String::new();
    }
    let sep = if unicode { " · " } else { " / " };
    let prefix = if use_nerd {
        "\u{f0e7} "
    } else if unicode {
        "⚡ "
    } else {
        "[sub] "
    };
    format!("{prefix}{}", parts.join(sep))
}

fn format_dashboard_url_segment(raw_url: Option<&str>, palette: &ThemePalette) -> String {
    let Some(raw) = raw_url else {
        return String::new();
    };
    let sanitized = sanitize_terminal_text(raw, 120);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return String::new();
    }
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return String::new();
    };
    if parsed.scheme() != "http" {
        return String::new();
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return String::new();
    }
    let Some(host) = parsed.host_str() else {
        return String::new();
    };
    if !host.eq_ignore_ascii_case("127.0.0.1") && !host.eq_ignore_ascii_case("localhost") {
        return String::new();
    }
    if !parsed.port_or_known_default().is_some_and(|p| p >= 1) {
        return String::new();
    }
    format!("{}{trimmed}{RESET}", palette.cyan)
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
    let show_git_branch = d.show_git_branch.unwrap_or(false);
    let show_current_dir = d.show_current_dir.unwrap_or(true);
    let show_username = d.show_username.unwrap_or(false);
    let show_daily_cost = d.show_daily_cost.unwrap_or(false);
    let show_weekly_cost = d.show_weekly_cost.unwrap_or(false);
    let show_speed = d.show_speed.unwrap_or(true);
    let show_cycle_mode = d.show_cycle_mode.unwrap_or(true);
    let show_breadcrumbs = d.show_breadcrumbs.unwrap_or(false);
    let show_dashboard_url = d.show_dashboard_url.unwrap_or(true);

    let text = get_language_text(resolved_lang);
    let palette = resolve_palette(&config.theme);

    let explicit_cols = term.columns.filter(|&c| c > 0).map(|c| c as usize);

    let glyph = if unicode {
        Glyphs {
            bar: "█",
            empty: "░",
            vbar: "│",
            hbar: "─",
            ellipsis: "…",
        }
    } else {
        Glyphs {
            bar: "#",
            empty: "-",
            vbar: "|",
            hbar: "-",
            ellipsis: "...",
        }
    };

    let mut branch_icon = if use_nerd {
        "\u{e725} ".to_string()
    } else if unicode {
        "⎇ ".to_string()
    } else {
        "[B] ".to_string()
    };

    if let Some(icons) = &config.icons
        && let Some(v) = &icons.branch
    {
        branch_icon = sanitize_terminal_text(v, 8);
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

    let mut cost_parts: Vec<String> = Vec::new();
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

    let max_idle_turns = d.model_speed_expire_turns.unwrap_or(30);
    let max_idle_ms = (d
        .model_speed_expire_minutes
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(5.0)
        * 60_000.0)
        .round() as i64;

    let main_model_ident = snapshot
        .model
        .as_ref()
        .map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.as_str()
            } else {
                m.id.as_str()
            }
        })
        .or(snapshot.last_turn_model.as_deref())
        .unwrap_or("");
    let main_model_key = compact_model_name(main_model_ident);

    let main_perf_turns_expired = snapshot.transcript_steps == 0
        && snapshot
            .main_model_turns_ago
            .is_some_and(|ago| ago > max_idle_turns);
    let main_speed_expired = main_perf_turns_expired
        || snapshot
            .main_model_last_active_ms
            .is_some_and(|ts| ts > 0 && (snapshot.observed_at_ms - ts) > max_idle_ms);

    let active_output_speed = if show_speed && !main_speed_expired {
        snapshot.output_speed_tok_s.filter(|s| s.is_finite() && *s > 0.0 && *s <= 5000.0)
    } else {
        None
    };
    let active_main_ttft = if show_speed && !main_speed_expired {
        snapshot.ttft_sec.and_then(|t| format_ttft(t, text.is_zh))
    } else {
        None
    };

    let display_child_speeds: Vec<&ChildModelSpeed> = if show_speed {
        snapshot
            .child_model_speeds
            .iter()
            .filter(|c| {
                if !c.speed_tok_s.is_finite() || c.speed_tok_s <= 0.0 || c.speed_tok_s > 5000.0 {
                    return false;
                }
                let norm = compact_model_name(&c.model);
                if !main_model_key.is_empty()
                    && (c.model == main_model_key || norm == main_model_key)
                {
                    return false;
                }
                if c.turns_ago > max_idle_turns {
                    return false;
                }
                if c.last_active_ms > 0 && (snapshot.observed_at_ms - c.last_active_ms) > max_idle_ms {
                    return false;
                }
                true
            })
            .take(3)
            .collect()
    } else {
        Vec::new()
    };

    let update_seg = match &snapshot.update_available_version {
        Some(ver) if !ver.trim().is_empty() => {
            let icon = if unicode { "⟳" } else { "[UP]" };
            format!("{}{icon} v{}{RESET}", palette.warning, ver.trim())
        }
        _ => String::new(),
    };

    let dashboard_url_seg = if show_dashboard_url {
        format_dashboard_url_segment(snapshot.dashboard_url.as_deref(), &palette)
    } else {
        String::new()
    };

    struct Line1Opts {
        child_limit: usize,
        include_child_ttft: bool,
        include_main_ttft: bool,
        include_child_speed: bool,
        include_update: bool,
        include_branch: bool,
        include_dir: bool,
        include_user: bool,
    }

    let assemble_line1 = |opts: &Line1Opts| -> String {
        let mut parts: Vec<String> = Vec::new();
        if opts.include_user && !username_seg.is_empty() {
            parts.push(username_seg.clone());
        }
        if opts.include_dir && !current_dir_seg.is_empty() {
            parts.push(current_dir_seg.clone());
        }
        if opts.include_branch && !branch_seg.is_empty() {
            parts.push(branch_seg.clone());
        }
        parts.push(plan_seg.clone());
        if !mode_seg.is_empty() {
            parts.push(mode_seg.clone());
        }
        if !cost_seg.is_empty() {
            parts.push(cost_seg.clone());
        }
        if show_speed {
            if let Some(speed) = active_output_speed {
                let ttft_suffix = if opts.include_main_ttft
                    && let Some(ref ttft) = active_main_ttft
                {
                    format!(" ({ttft})")
                } else {
                    String::new()
                };
                parts.push(format!(
                    "{}{}: {speed:.1} {}{ttft_suffix}{RESET}",
                    palette.secondary, text.out_label, text.tok_per_sec
                ));
            } else if opts.include_main_ttft
                && let Some(ref ttft) = active_main_ttft
            {
                parts.push(format!("{}{ttft}{RESET}", palette.secondary));
            }
            if opts.include_child_speed && !display_child_speeds.is_empty() {
                let lim = opts.child_limit.min(display_child_speeds.len());
                let raw_child = format_child_model_speeds_seg(
                    &display_child_speeds[..lim],
                    unicode,
                    use_nerd,
                    opts.include_child_ttft,
                    text,
                );
                if !raw_child.is_empty() {
                    parts.push(format!("{}{raw_child}{RESET}", palette.magenta));
                }
            }
        }
        if opts.include_update && !update_seg.is_empty() {
            parts.push(update_seg.clone());
        }
        parts.join(&divider)
    };

    let mut line1_opts = Line1Opts {
        child_limit: display_child_speeds.len(),
        include_child_ttft: true,
        include_main_ttft: true,
        include_child_speed: true,
        include_update: true,
        include_branch: true,
        include_dir: true,
        include_user: true,
    };

    let mut line1 = assemble_line1(&line1_opts);
    if let Some(cols) = explicit_cols
        && ansi_display_width(&line1) > cols
    {
        let mut step = 0u8;
        while step <= 8 {
            let applied = match step {
                0 if display_child_speeds.len() > 2 => {
                    line1_opts.child_limit = 2;
                    true
                }
                1 if display_child_speeds.len() > 1 => {
                    line1_opts.child_limit = 1;
                    true
                }
                2 => {
                    line1_opts.include_child_ttft = false;
                    true
                }
                3 => {
                    line1_opts.include_main_ttft = false;
                    true
                }
                4 => {
                    line1_opts.include_child_speed = false;
                    true
                }
                5 => {
                    line1_opts.include_update = false;
                    true
                }
                6 => {
                    line1_opts.include_branch = false;
                    true
                }
                7 => {
                    line1_opts.include_dir = false;
                    true
                }
                8 => {
                    line1_opts.include_user = false;
                    true
                }
                _ => false,
            };
            step += 1;
            if !applied {
                continue;
            }
            line1 = assemble_line1(&line1_opts);
            if ansi_display_width(&line1) <= cols {
                break;
            }
        }
        if ansi_display_width(&line1) > cols {
            line1 = truncate_ansi_to_display_width(&line1, cols, glyph.ellipsis);
        }
    }

    // Line 2 (Image quota exhaustion alert, if active)
    let mut line2 = String::new();
    if let Some(ex) = &snapshot.image_exhausted {
        let secs_left = ((ex.reset_time_ms - snapshot.observed_at_ms) as f64 / 1000.0).round() as i64;
        if secs_left > 0 {
            let h = secs_left / 3600;
            let m = (secs_left % 3600) / 60;
            let icon = if unicode { "⚠️ " } else { "[!] " };
            line2 = format!(
                "{}{icon}Image Quota Exhausted (Resets in: {h:02}h{m:02}m){RESET}",
                palette.critical
            );
            if let Some(cols) = explicit_cols
                && ansi_display_width(&line2) > cols
            {
                line2 = truncate_ansi_to_display_width(&line2, cols, glyph.ellipsis);
            }
        }
    }

    // Line 3 (Workspace metadata: memory file / breadcrumbs, rules, MCPs, hooks)
    let mut meta_parts: Vec<String> = Vec::new();
    let breadcrumb_count = d.breadcrumb_count.unwrap_or(3) as usize;
    if show_breadcrumbs && breadcrumb_count > 0 {
        if !snapshot.workspace_meta.breadcrumbs.is_empty() {
            let start = snapshot
                .workspace_meta
                .breadcrumbs
                .len()
                .saturating_sub(breadcrumb_count);
            for item in &snapshot.workspace_meta.breadcrumbs[start..] {
                let clean = sanitize_terminal_text(item, 80);
                if !clean.is_empty() {
                    meta_parts.push(format!("{}{clean}{RESET}", palette.secondary));
                }
            }
        } else if let Some(mem) = &snapshot.workspace_meta.memory_file {
            let clean = sanitize_terminal_text(mem, 80);
            if !clean.is_empty() {
                meta_parts.push(format!("{}1 {clean}{RESET}", palette.secondary));
            }
        }
    }
    if snapshot.workspace_meta.rules_count > 0 {
        meta_parts.push(format!(
            "{}{} rules{RESET}",
            palette.secondary, snapshot.workspace_meta.rules_count
        ));
    }
    if snapshot.workspace_meta.mcp_count > 0 {
        meta_parts.push(format!(
            "{}{} MCPs{RESET}",
            palette.secondary, snapshot.workspace_meta.mcp_count
        ));
    }
    if snapshot.workspace_meta.hooks_count > 0 {
        meta_parts.push(format!(
            "{}{} hooks{RESET}",
            palette.secondary, snapshot.workspace_meta.hooks_count
        ));
    }
    let mut line3 = meta_parts.join(&divider);
    if let Some(cols) = explicit_cols
        && !line3.is_empty()
        && ansi_display_width(&line3) > cols
    {
        line3 = truncate_ansi_to_display_width(&line3, cols, glyph.ellipsis);
    }

    let mut lines: Vec<String> = Vec::new();
    if !line1.is_empty() {
        lines.push(line1);
    }
    if !line2.is_empty() {
        lines.push(line2);
    }
    if !line3.is_empty() {
        lines.push(line3);
    }

    if let Some(quota_block) =
        render_quota_section(snapshot, config, term, &palette, &glyph, &divider)
        && !quota_block.is_empty()
    {
        lines.push(quota_block);
    }

    let show_context_monitor = d.show_context_monitor.unwrap_or(true);
    let show_model_cache = d.show_model_cache.unwrap_or(true);

    if show_model_cache
        && let Some(cache_line) = render_model_cache_line(snapshot, config, term, text, &palette)
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

    if !dashboard_url_seg.is_empty()
        && explicit_cols.is_none_or(|cols| ansi_display_width(&dashboard_url_seg) <= cols)
    {
        lines.push(dashboard_url_seg);
    }

    lines.join("\n")
}
