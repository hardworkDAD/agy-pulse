//! Pure in-memory domain model and calculations for host-provided model quota windows.

/// Window duration bucket for a provider quota observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWindowKind {
    FiveHour,
    Weekly,
}

/// Single quota window observation parsed from host standard input.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    pub remaining_fraction: f64,
    pub reset_time_ms: Option<i64>,
}

/// Selected binding quota window for a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct CriticalWindow {
    pub kind: QuotaWindowKind,
    pub window: QuotaWindow,
}

/// 5-hour and weekly quota windows for a single model provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderQuota {
    pub five_hour: Option<QuotaWindow>,
    pub weekly: Option<QuotaWindow>,
}

/// Extra dynamic provider quota entry (excluding filtered OpenAI keys).
#[derive(Debug, Clone, PartialEq)]
pub struct NamedProviderQuota {
    pub key: String,
    pub quota: ProviderQuota,
}

/// Normalized provider-level quota summary extracted from host standard input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderQuotaSummary {
    pub google: ProviderQuota,
    pub claude: ProviderQuota,
    pub argon: ProviderQuota,
    pub extras: Vec<NamedProviderQuota>,
}

/// Format a remaining fraction (`0.0..=1.0`) into an integer percentage.
/// Clamps non-100% fractions (e.g. `0.9953883`) to at most `99` so fractional
/// usage never rounds up to `100%` and looks stuck.
pub fn format_quota_percent(remaining_fraction: f64) -> u8 {
    if !remaining_fraction.is_finite() || remaining_fraction <= 0.0 {
        return 0;
    }
    if remaining_fraction >= 1.0 {
        return 100;
    }
    (remaining_fraction * 100.0).floor().clamp(0.0, 99.0) as u8
}

/// Format a countdown duration in seconds into compact form (`10d`, `4d3h`, `12h`, `4h59m`, `22m`, `now`).
pub fn format_reset_duration(secs: i64) -> String {
    if secs <= 0 {
        return "now".to_string();
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d >= 10 {
        format!("{d}d")
    } else if d > 0 {
        format!("{d}d{h}h")
    } else if h >= 10 {
        format!("{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}

/// Compute `~<duration>` countdown text relative to `observed_at_ms`, or `None` if missing or expired.
pub fn format_reset_countdown(reset_time_ms: Option<i64>, observed_at_ms: i64) -> Option<String> {
    let reset_ms = reset_time_ms?;
    if reset_ms <= observed_at_ms {
        return None;
    }
    let secs_left = ((reset_ms - observed_at_ms) as f64 / 1000.0)
        .round()
        .max(0.0) as i64;
    Some(format!("~{}", format_reset_duration(secs_left)))
}

/// Returns `true` when a window has a `reset_time_ms` at or before `observed_at_ms`.
pub fn is_quota_window_expired(window: &QuotaWindow, observed_at_ms: i64) -> bool {
    window
        .reset_time_ms
        .is_some_and(|reset_ms| reset_ms <= observed_at_ms)
}

/// Drop expired windows from a `ProviderQuota` relative to `observed_at_ms`.
pub fn prune_expired_provider_quota(
    provider: &ProviderQuota,
    observed_at_ms: i64,
) -> ProviderQuota {
    ProviderQuota {
        five_hour: provider
            .five_hour
            .as_ref()
            .filter(|w| !is_quota_window_expired(w, observed_at_ms))
            .cloned(),
        weekly: provider
            .weekly
            .as_ref()
            .filter(|w| !is_quota_window_expired(w, observed_at_ms))
            .cloned(),
    }
}

/// Pick the binding (lower remaining fraction) non-expired window for a provider.
/// Ties break in favor of the 5-hour window.
pub fn pick_critical_window(
    provider: &ProviderQuota,
    observed_at_ms: i64,
) -> Option<CriticalWindow> {
    let pruned = prune_expired_provider_quota(provider, observed_at_ms);
    match (pruned.five_hour, pruned.weekly) {
        (Some(five), Some(week)) => {
            if five.remaining_fraction <= week.remaining_fraction {
                Some(CriticalWindow {
                    kind: QuotaWindowKind::FiveHour,
                    window: five,
                })
            } else {
                Some(CriticalWindow {
                    kind: QuotaWindowKind::Weekly,
                    window: week,
                })
            }
        }
        (Some(five), None) => Some(CriticalWindow {
            kind: QuotaWindowKind::FiveHour,
            window: five,
        }),
        (None, Some(week)) => Some(CriticalWindow {
            kind: QuotaWindowKind::Weekly,
            window: week,
        }),
        (None, None) => None,
    }
}

/// Returns `true` if a raw provider key belongs to the filtered OpenAI/Codex/GPT family.
pub fn is_openai_provider_key(key: &str) -> bool {
    let lower = key.trim().to_ascii_lowercase();
    lower == "openai"
        || lower.starts_with("openai")
        || lower.starts_with("codex")
        || lower.starts_with("gpt")
}

/// Returns `true` if at least one non-OpenAI provider holds a non-expired quota window.
pub fn has_valid_provider_quota(summary: &ProviderQuotaSummary, observed_at_ms: i64) -> bool {
    pick_critical_window(&summary.google, observed_at_ms).is_some()
        || pick_critical_window(&summary.claude, observed_at_ms).is_some()
        || pick_critical_window(&summary.argon, observed_at_ms).is_some()
        || summary
            .extras
            .iter()
            .any(|e| !is_openai_provider_key(&e.key) && pick_critical_window(&e.quota, observed_at_ms).is_some())
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if is_leap_year(year) { 29 } else { 28 }),
        _ => None,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * m_adj + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse_two_digits(s: &str) -> Option<u32> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok()
}

/// Pure RFC3339 / ISO-8601 timestamp parser returning UTC epoch milliseconds.
pub fn parse_iso8601_to_epoch_ms(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.len() < 10 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year_str = s.get(0..4)?;
    if !year_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year = year_str.parse::<i64>().ok()?;
    let month = parse_two_digits(s.get(5..7)?)?;
    let day = parse_two_digits(s.get(8..10)?)?;
    let max_day = days_in_month(year, month)?;
    if day == 0 || day > max_day {
        return None;
    }

    let days = days_from_civil(year, month, day);
    if s.len() == 10 {
        return days.checked_mul(86_400_000);
    }

    let sep = bytes[10];
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    let rest = s.get(11..)?;
    if rest.len() < 8 {
        return None;
    }
    let rbytes = rest.as_bytes();
    if rbytes.get(2) != Some(&b':') || rbytes.get(5) != Some(&b':') {
        return None;
    }
    let hour = parse_two_digits(rest.get(0..2)?)?;
    let minute = parse_two_digits(rest.get(3..5)?)?;
    let second = parse_two_digits(rest.get(6..8)?)?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let mut tail = rest.get(8..)?;
    let mut millis: i64 = 0;
    if let Some(after_dot) = tail.strip_prefix('.') {
        let frac_len = after_dot
            .bytes()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if frac_len == 0 {
            return None;
        }
        let frac_str = &after_dot[..frac_len];
        let mut padded = [b'0'; 3];
        for (idx, b) in frac_str.bytes().take(3).enumerate() {
            padded[idx] = b;
        }
        millis = std::str::from_utf8(&padded).ok()?.parse::<i64>().ok()?;
        tail = &after_dot[frac_len..];
    }

    let tz_offset_ms: i64 = if tail.is_empty() || tail.eq_ignore_ascii_case("z") {
        0
    } else {
        let sign: i64 = if tail.starts_with('+') {
            1
        } else if tail.starts_with('-') {
            -1
        } else {
            return None;
        };
        let tz_body = &tail[1..];
        let (tz_h, tz_m) = if tz_body.len() == 5 && tz_body.as_bytes()[2] == b':' {
            (
                parse_two_digits(&tz_body[0..2])?,
                parse_two_digits(&tz_body[3..5])?,
            )
        } else if tz_body.len() == 4 {
            (
                parse_two_digits(&tz_body[0..2])?,
                parse_two_digits(&tz_body[2..4])?,
            )
        } else {
            return None;
        };
        if tz_h > 23 || tz_m > 59 {
            return None;
        }
        sign * ((tz_h as i64 * 3600 + tz_m as i64 * 60) * 1000)
    };

    let day_ms = days.checked_mul(86_400_000)?;
    let time_ms = ((hour as i64 * 3600 + minute as i64 * 60 + second as i64) * 1000) + millis;
    day_ms.checked_add(time_ms)?.checked_sub(tz_offset_ms)
}
