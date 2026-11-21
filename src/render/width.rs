use std::sync::OnceLock;

use regex::Regex;
use unicode_width::UnicodeWidthChar;

/// Strip ANSI OSC and CSI escape sequences from a string.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            if bytes[i + 1] == b'[' {
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
                continue;
            }
            if bytes[i + 1] == b']' {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        if let Some(ch) = input[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// Remove ANSI sequences and control characters, truncating to `max_codepoints`.
pub(super) fn sanitize_terminal_text(value: &str, max_codepoints: usize) -> String {
    let stripped = strip_ansi(value);
    let mut out = String::with_capacity(stripped.len());
    let mut count = 0;
    for ch in stripped.chars() {
        if (ch as u32) < 0x20 || ch == '\x7F' {
            continue;
        }
        if count >= max_codepoints {
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn char_display_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp == 0 {
        return 0;
    }
    if (0x0300..=0x036F).contains(&cp)
        || (0x1AB0..=0x1AFF).contains(&cp)
        || (0x1DC0..=0x1DFF).contains(&cp)
        || (0x20D0..=0x20FF).contains(&cp)
        || (0xFE20..=0xFE2F).contains(&cp)
        || (0xFE00..=0xFE0F).contains(&cp)
        || (0xE0100..=0xE01EF).contains(&cp)
        || (0x200B..=0x200F).contains(&cp)
        || cp == 0x2060
        || cp == 0xFEFF
        || (0x1F3FB..=0x1F3FF).contains(&cp)
    {
        return 0;
    }
    if (0x1F300..=0x1FAF8).contains(&cp) {
        return 2;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn unit_display_width(chars: &[char], idx: usize) -> usize {
    let base = char_display_width(chars[idx]);
    let next = chars.get(idx + 1).copied();
    if next == Some('\u{FE0F}') && base == 1 {
        2
    } else {
        base
    }
}

/// Compute visual terminal column width for a plain string (handling CJK and Emoji).
pub fn display_width(input: &str) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut width = 0;
    for idx in 0..chars.len() {
        width += unit_display_width(&chars, idx);
    }
    width
}

/// Compute visual terminal column width ignoring ANSI escape sequences.
pub fn ansi_display_width(input: &str) -> usize {
    display_width(&strip_ansi(input))
}

/// Truncate an ANSI-colored string to at most `max_width` display columns, preserving
/// escape sequences seen before the cut and closing cleanly with `ellipsis` + `\x1b[0m`.
pub fn truncate_ansi_to_display_width(input: &str, max_width: usize, ellipsis: &str) -> String {
    if max_width == 0 {
        return String::new();
    }
    if ansi_display_width(input) <= max_width {
        return input.to_string();
    }

    let ellipsis_width = display_width(ellipsis);
    let mut budget = max_width.saturating_sub(ellipsis_width);
    let safe_ellipsis = if ellipsis_width > max_width {
        budget = 0;
        let e_chars: Vec<char> = ellipsis.chars().collect();
        let mut e_out = String::new();
        let mut e_used = 0;
        for idx in 0..e_chars.len() {
            let w = unit_display_width(&e_chars, idx);
            if e_used + w > max_width {
                break;
            }
            e_out.push(e_chars[idx]);
            e_used += w;
        }
        e_out
    } else {
        ellipsis.to_string()
    };

    let mut out = String::with_capacity(input.len() + 8);
    let mut used = 0;
    let mut truncated = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            if bytes[i + 1] == b'[' {
                let start = i;
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
                out.push_str(&input[start..i]);
                continue;
            }
            if bytes[i + 1] == b']' {
                let start = i;
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                out.push_str(&input[start..i]);
                continue;
            }
        }

        // Collect a maximal contiguous non-escape text slice so VS16 lookahead works cleanly.
        let text_start = i;
        while i < bytes.len() {
            if bytes[i] == 0x1B && i + 1 < bytes.len() && (bytes[i + 1] == b'[' || bytes[i + 1] == b']') {
                break;
            }
            if let Some(ch) = input[i..].chars().next() {
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        let chars: Vec<char> = input[text_start..i].chars().collect();
        for idx in 0..chars.len() {
            let w = unit_display_width(&chars, idx);
            if used + w > budget {
                truncated = true;
                break;
            }
            out.push(chars[idx]);
            used += w;
        }
        if truncated {
            break;
        }
    }

    format!("{out}{safe_ellipsis}\x1b[0m")
}

/// Truncate plain text to `width` display columns (with `"..."`) or right-pad with spaces.
pub(super) fn truncate_and_pad(input: &str, width: usize) -> String {
    let current_width = display_width(input);
    if current_width <= width {
        return format!("{input}{}", " ".repeat(width.saturating_sub(current_width)));
    }
    let ellipsis = "...";
    let ellipsis_width = display_width(ellipsis);
    let budget = width.saturating_sub(ellipsis_width);
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut used = 0;
    for idx in 0..chars.len() {
        let w = unit_display_width(&chars, idx);
        if used + w > budget {
            break;
        }
        out.push(chars[idx]);
        used += w;
    }
    let result = format!("{out}{ellipsis}");
    let res_width = display_width(&result);
    format!("{result}{}", " ".repeat(width.saturating_sub(res_width)))
}

/// Format token count into compact lowercase form (`1.2M`, `150k`, `42`).
pub(super) fn format_tokens(n: u64) -> String {
    let val = n as f64;
    if val >= 999_950.0 {
        let mut s = format!("{:.1}", val / 1_000_000.0);
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        return format!("{s}M");
    }
    if val >= 999.5 {
        let mut s = format!("{:.1}", val / 1_000.0);
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        if s == "1000" {
            return "1M".to_string();
        }
        return format!("{s}k");
    }
    n.to_string()
}

/// Format token count into uppercase K/M units for context monitor line (`1.2M`, `150K`, `42`).
pub(super) fn format_monitor_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let mut s = format!("{:.1}", n as f64 / 1_000_000.0);
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        return format!("{s}M");
    }
    if n >= 1_000 {
        let mut s = format!("{:.1}", n as f64 / 1_000.0);
        if s == "1000.0" || s == "1000" {
            return "1M".to_string();
        }
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        return format!("{s}K");
    }
    n.to_string()
}

fn tier_abbrev(tier: &str) -> String {
    match tier {
        "XHigh" => "XH".to_string(),
        "Thinking" => "Th".to_string(),
        "High" => "H".to_string(),
        "Medium" => "M".to_string(),
        "Low" => "L".to_string(),
        "Extra-Low" => "XL".to_string(),
        other => other.chars().next().map(|c| c.to_string()).unwrap_or_default(),
    }
}

/// Abbreviate full model display name for line 1 (e.g. `Claude Opus 4.6 (Thinking)` -> `Opus 4.6(Th)`).
pub(super) fn simplify_model_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    static RE_GEMINI: OnceLock<Option<Regex>> = OnceLock::new();
    static RE_ORCH: OnceLock<Option<Regex>> = OnceLock::new();
    static RE_CLAUDE_FAM_VER: OnceLock<Option<Regex>> = OnceLock::new();
    static RE_CLAUDE_VER_FAM: OnceLock<Option<Regex>> = OnceLock::new();
    static RE_GPT_OSS: OnceLock<Option<Regex>> = OnceLock::new();

    if let Some(re) = RE_GEMINI.get_or_init(|| {
        Regex::new(r"^Gemini ([\d.]+) (Flash|Pro|Argon|Carbon|Hydrogen|Neon) \(([\w-]+)\)").ok()
    }) && let Some(caps) = re.captures(name)
    {
        let ver = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let fam = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let tier = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        let replacement = format!("Gemini {ver} {fam}({})", tier_abbrev(tier));
        return name.replacen(full_match, &replacement, 1);
    }

    if let Some(re) = RE_ORCH.get_or_init(|| Regex::new(r"^Orchestrator \(([^)]+)\)").ok())
        && let Some(caps) = re.captures(name)
    {
        let fam = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        return name.replacen(full_match, &format!("Orch ({fam})"), 1);
    }

    if let Some(re) = RE_CLAUDE_FAM_VER
        .get_or_init(|| Regex::new(r"^Claude (\w+) ([\d.]+) \(([\w-]+)\)").ok())
        && let Some(caps) = re.captures(name)
    {
        let fam = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let ver = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let tier = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        let replacement = format!("{fam} {ver}({})", tier_abbrev(tier));
        return name.replacen(full_match, &replacement, 1);
    }

    if let Some(re) = RE_CLAUDE_VER_FAM
        .get_or_init(|| Regex::new(r"^Claude ([\d.]+) (\w+)(?: \(([\w-]+)\))?").ok())
        && let Some(caps) = re.captures(name)
    {
        let ver = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let fam = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        let replacement = if let Some(tier) = caps.get(3).map(|m| m.as_str()) {
            format!("{fam} {ver}({})", tier_abbrev(tier))
        } else {
            format!("{fam} {ver}")
        };
        return name.replacen(full_match, &replacement, 1);
    }

    if let Some(re) = RE_GPT_OSS.get_or_init(|| Regex::new(r"^GPT-OSS (.+?) \(([\w-]+)\)").ok())
        && let Some(caps) = re.captures(name)
    {
        let spec = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        return name.replacen(full_match, &format!("GPT-OSS {spec}"), 1);
    }

    name.to_string()
}
