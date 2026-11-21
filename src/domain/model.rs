use super::quota::ProviderQuotaSummary;

/// Pure in-memory domain value types representing a single statusline pulse snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelIdentity {
    pub id: String,
    pub display_name: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_exclusive: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextWindowSnapshot {
    pub context_window_size: u64,
    pub used_percentage: Option<f64>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: Option<u64>,
    pub total_cache_creation_tokens: Option<u64>,
    pub current_usage: Option<TokenUsage>,
    pub has_total_input: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub current_dir: String,
    pub git_branch: Option<String>,
}

/// Offline-discovered workspace and host configuration metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceMetadata {
    pub memory_file: Option<String>,
    pub breadcrumbs: Vec<String>,
    pub rules_count: u64,
    pub mcp_count: u64,
    pub hooks_count: u64,
}

/// Historical peak active turn tokens tracked separately by cache-inclusive vs cache-exclusive mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnPeakByMode {
    pub inclusive_peak: u64,
    pub exclusive_peak: u64,
}

/// Per-turn prompt cache hit metrics for statusline display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnCacheMetrics {
    pub available: bool,
    pub cache_read: u64,
    pub total_prompt: u64,
    pub hit_rate: f64,
}

/// Active child/swarm model rolling output speed entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChildModelSpeed {
    pub model: String,
    pub speed_tok_s: f64,
    pub ttft_sec: Option<f64>,
    pub turns_ago: u64,
    pub last_active_ms: i64,
}

/// Cumulative per-model token totals from transcript scanning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelTokenTotals {
    pub model_key: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub steps: u64,
}

/// Active image model rate-limit exhaustion window extracted from structured transcript errors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageQuotaExhausted {
    pub reset_time_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostIdentity {
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
    pub plan_tier: Option<String>,
    pub cycle_mode: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostSnapshot {
    pub native_total_usd: Option<f64>,
    pub cascade_usd: Option<f64>,
    pub today_usd: Option<f64>,
    pub week_usd: Option<f64>,
}

/// Complete immutable snapshot passed from `ingest` into `render` and `domain` calculators.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PulseSnapshot {
    pub has_conversation_identity: bool,
    pub model: Option<ModelIdentity>,
    pub host: HostIdentity,
    pub workspace: WorkspaceInfo,
    pub workspace_meta: WorkspaceMetadata,
    pub context: Option<ContextWindowSnapshot>,
    pub cost: CostSnapshot,
    pub terminal_width: Option<u16>,
    pub task_count: u64,
    pub compaction_count: u64,
    pub server_compaction_limit: Option<u64>,
    pub peak_raw_turn_tokens: u64,
    pub turn_history_max_by_mode: TurnPeakByMode,
    pub last_turn_usage: Option<TokenUsage>,
    pub last_turn_model: Option<String>,
    pub max_historical_cache: u64,
    pub turn_cache_metrics: Option<TurnCacheMetrics>,
    pub transcript_total_input_tokens: u64,
    pub transcript_total_output_tokens: u64,
    pub output_speed_tok_s: Option<f64>,
    pub main_model_last_active_ms: Option<i64>,
    pub child_model_speeds: Vec<ChildModelSpeed>,
    pub model_token_totals: Vec<ModelTokenTotals>,
    pub image_exhausted: Option<ImageQuotaExhausted>,
    pub ttft_sec: Option<f64>,
    pub update_available_version: Option<String>,
    pub observed_at_ms: i64,
    pub quota: Option<ProviderQuotaSummary>,
}

const MODEL_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("gemini-3-flash-agent", "Gemini 3.5 Flash (High)"),
    ("gemini-3.5-flash-low", "Gemini 3.5 Flash (Medium)"),
    ("gemini-3.5-flash-extra-low", "Gemini 3.5 Flash (Low)"),
    ("gemini-pro-agent", "Gemini 3.1 Pro (High)"),
    ("gemini-3.1-pro-low", "Gemini 3.1 Pro (Low)"),
    ("gemini-4-argon", "Gemini 4 Argon (Medium)"),
    ("gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
    ("gemini-4-carbon-medium", "Gemini 4 Carbon (Medium)"),
    ("gemini-4-carbon-low", "Gemini 4 Carbon (Low)"),
    ("gemini-4-hydrogen-xhigh", "Gemini 4 Hydrogen (XHigh)"),
    ("gemini-4-hydrogen-high", "Gemini 4 Hydrogen (High)"),
    ("gemini-4-hydrogen-medium", "Gemini 4 Hydrogen (Medium)"),
    ("gemini-4-hydrogen-low", "Gemini 4 Hydrogen (Low)"),
    ("gemini-4-hydrogen-sonar", "Gemini 4 Hydrogen (Sonar)"),
    ("gemini-4-neon", "Gemini 4 Neon"),
    ("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)"),
    ("gemini-4-neon-high", "Gemini 4 Neon (High)"),
    ("gemini-4-neon-medium", "Gemini 4 Neon (Medium)"),
    ("gemini-4-neon-low", "Gemini 4 Neon (Low)"),
    ("gemini-4-argon-high", "Gemini 4 Argon (High)"),
    ("gemini-4-argon-medium", "Gemini 4 Argon (Medium)"),
    ("gemini-4-argon-low", "Gemini 4 Argon (Low)"),
    ("gemini-3.8-flash-high", "Gemini 3.8 Flash (High)"),
    ("gemini-3.8-flash-medium", "Gemini 3.8 Flash (Medium)"),
    ("gemini-3.8-flash-low", "Gemini 3.8 Flash (Low)"),
    ("gemini-3.8-flash-tiered", "Gemini 3.8 Flash (Tiered)"),
    ("gemini-3.5-flash-lite", "Gemini 3.5 Flash Lite"),
    ("gemini-3.1-flash-image", "Gemini 3.1 Flash Image"),
    ("gemini-pro-orchestrator", "Orchestrator (Carbon)"),
    ("gemini-pro-orchestrator-argon", "Orchestrator (Argon)"),
    ("gemini-pro-orchestrator-neon", "Orchestrator (Neon)"),
    ("claude-opus-4-6-thinking", "Claude Opus 4.6 (Thinking)"),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6 (Thinking)"),
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-opus-5-5-thinking", "Claude Opus 5.5 (Thinking)"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-sonnet-5-5-thinking", "Claude Sonnet 5.5 (Thinking)"),
    ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"),
];

/// Resolve an authoritative human-readable model display name from an ID and optional explicit name.
pub fn resolve_model_display_name(id: &str, explicit_display_name: Option<&str>) -> String {
    if let Some(explicit) = explicit_display_name {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() && trimmed != id {
            return trimmed.to_string();
        }
    }
    for (known_id, display) in MODEL_DISPLAY_NAMES {
        if *known_id == id {
            return (*display).to_string();
        }
    }
    if let Some(explicit) = explicit_display_name {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    id.to_string()
}

/// Returns true if the non-Anthropic model family reports `input_tokens` inclusive of cache read/write tokens by default.
pub fn model_includes_cache_in_input(name_or_id: &str) -> bool {
    let lower = name_or_id.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if ["claude", "anthropic", "sonnet", "opus", "haiku"]
        .iter()
        .any(|k| lower.contains(k))
    {
        return false;
    }
    const SUBSTRINGS: &[&str] = &[
        "gemini",
        "carbon",
        "hydrogen",
        "argon",
        "neon",
        "orchestrator",
        "gpt",
        "deepseek",
        "o1-",
        "o3-",
        "o4-",
    ];
    if SUBSTRINGS.iter().any(|s| lower.contains(s)) {
        return true;
    }
    contains_word(&lower, "hydro")
        || contains_word(&lower, "orch-c")
        || contains_word(&lower, "orch-a")
        || contains_word(&lower, "orch-n")
}

/// Single authoritative predicate determining whether `usage.input_tokens` already includes
/// `cache_read_input_tokens + cache_creation_input_tokens`, shared by `accounting` and `render`.
pub fn is_usage_cache_inclusive(model_name: &str, usage: &TokenUsage) -> bool {
    if usage.cache_exclusive {
        return false;
    }
    let cache_total = usage
        .cache_read_input_tokens
        .saturating_add(usage.cache_creation_input_tokens);
    if usage.input_tokens < cache_total {
        return false;
    }
    if model_name.is_empty() || model_includes_cache_in_input(model_name) {
        return true;
    }
    let lower = model_name.to_ascii_lowercase();
    let looks_anthropic = ["claude", "anthropic", "sonnet", "opus", "haiku"]
        .iter()
        .any(|k| lower.contains(k));
    looks_anthropic && cache_total > 0 && usage.input_tokens >= cache_total
}

/// Calculate per-turn prompt cache hit rate respecting model cache-inclusive semantics.
pub fn calculate_turn_cache_metrics(usage: &TokenUsage, model_name: &str) -> TurnCacheMetrics {
    let cache_read = usage.cache_read_input_tokens;
    let cache_write = usage.cache_creation_input_tokens;
    let cache_total = cache_read.saturating_add(cache_write);
    let inclusive = is_usage_cache_inclusive(model_name, usage);
    let total_prompt = if inclusive {
        usage.input_tokens
    } else {
        usage.input_tokens.saturating_add(cache_total)
    };
    if total_prompt == 0 {
        return TurnCacheMetrics::default();
    }
    let hit_rate = ((cache_read as f64 * 100.0) / total_prompt as f64).clamp(0.0, 100.0);
    TurnCacheMetrics {
        available: true,
        cache_read,
        total_prompt,
        hit_rate,
    }
}

/// Produce a compact family label for a model name or ID (e.g. `Gemini 4 Carbon (High)` -> `Carbon`).
pub fn compact_model_name(display_name: &str) -> String {
    if display_name.is_empty() {
        return String::new();
    }
    let lower = display_name.to_ascii_lowercase();
    if lower.contains("orchestrator") {
        if lower.contains("argon") {
            return "Orch-A".to_string();
        }
        if lower.contains("neon") {
            return "Orch-N".to_string();
        }
        return "Orch-C".to_string();
    }
    if lower.contains("hydrogen") {
        return "Hydro".to_string();
    }
    if lower.contains("carbon") {
        return "Carbon".to_string();
    }
    if lower.contains("argon") {
        return "Argon".to_string();
    }
    if lower.contains("neon") {
        return "Neon".to_string();
    }
    if lower.contains("flash") {
        return "Flash".to_string();
    }
    if lower.contains("sonnet") {
        return "Sonnet".to_string();
    }
    if lower.contains("opus") {
        return "Opus".to_string();
    }
    if lower.contains("haiku") {
        return "Haiku".to_string();
    }
    if lower.contains("gpt") {
        return "GPT".to_string();
    }
    if lower.contains("deepseek") {
        return "DeepSeek".to_string();
    }
    if lower.contains("pro") {
        return "Pro".to_string();
    }
    display_name.chars().take(6).collect()
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0
            || !haystack[..abs]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let after = abs + word.len();
        let after_ok = after == haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return true;
        }
        start = abs + word.len();
    }
    false
}
