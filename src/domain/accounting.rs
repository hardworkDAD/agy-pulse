use super::model::{PulseSnapshot, TokenUsage, is_usage_cache_inclusive};
use super::pricing::calculate_token_cost;

/// Returns true if the model ID uses AWS Bedrock routing prefixes.
pub fn is_bedrock_model_id(id: &str) -> bool {
    id.starts_with("anthropic.")
        || id.starts_with("us.anthropic.")
        || id.starts_with("eu.anthropic.")
}

/// Returns true if the model ID uses Google Vertex AI `@` routing syntax.
pub fn is_vertex_model_id(id: &str) -> bool {
    id.starts_with("claude-") && id.contains('@')
}

/// Filter and validate native cost reported by the host CLI.
pub fn resolve_native_cost_usd(
    raw_usd: Option<f64>,
    model_id: &str,
    allow_routed_cost: bool,
) -> Option<f64> {
    let cost = raw_usd?;
    if !cost.is_finite() || cost <= 0.0 {
        return None;
    }
    let routed = is_bedrock_model_id(model_id) || is_vertex_model_id(model_id);
    if routed && !allow_routed_cost {
        return None;
    }
    Some(cost)
}

/// Compute single authoritative cascade cost from native cost or token fallback.
pub fn resolve_cascade_cost_usd(snapshot: &PulseSnapshot, allow_routed_cost: bool) -> Option<f64> {
    let model_id = snapshot
        .model
        .as_ref()
        .map(|m| m.id.as_str())
        .unwrap_or("");
    if let Some(native) = resolve_native_cost_usd(
        snapshot.cost.native_total_usd,
        model_id,
        allow_routed_cost,
    ) {
        return Some(native);
    }

    if let Some(cascade_usd) = snapshot.cost.cascade_usd
        && cascade_usd.is_finite()
        && cascade_usd > 0.0
    {
        return Some(cascade_usd);
    }

    let cw = snapshot.context.as_ref()?;
    let cur = cw.current_usage.as_ref();
    let model_name = snapshot
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

    let input_tokens = if cw.has_total_input {
        cw.total_input_tokens
    } else {
        cur.map(|u| u.input_tokens).unwrap_or(0)
    };
    let output_tokens = if cw.total_output_tokens > 0 {
        cw.total_output_tokens
    } else {
        cur.map(|u| u.output_tokens).unwrap_or(0)
    };

    let (cache_read_tokens, cache_creation_tokens) = if cw.has_total_input {
        if cw.total_cache_read_tokens.is_some() || cw.total_cache_creation_tokens.is_some() {
            (
                cw.total_cache_read_tokens.unwrap_or(0),
                cw.total_cache_creation_tokens.unwrap_or(0),
            )
        } else if let Some(u) = cur
            && u.input_tokens > 0
            && cw.total_input_tokens > u.input_tokens
        {
            let ratio_read = ((u.cache_read_input_tokens as f64 / u.input_tokens as f64)
                * cw.total_input_tokens as f64)
                .round() as u64;
            let read = cw.total_input_tokens.min(ratio_read);
            let ratio_create = ((u.cache_creation_input_tokens as f64 / u.input_tokens as f64)
                * cw.total_input_tokens as f64)
                .round() as u64;
            let create = cw.total_input_tokens.saturating_sub(read).min(ratio_create);
            (read, create)
        } else {
            (
                cur.map(|u| u.cache_read_input_tokens).unwrap_or(0),
                cur.map(|u| u.cache_creation_input_tokens).unwrap_or(0),
            )
        }
    } else {
        (
            cw.total_cache_read_tokens
                .or_else(|| cur.map(|u| u.cache_read_input_tokens))
                .unwrap_or(0),
            cw.total_cache_creation_tokens
                .or_else(|| cur.map(|u| u.cache_creation_input_tokens))
                .unwrap_or(0),
        )
    };

    let usage = TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens: cache_read_tokens,
        cache_creation_input_tokens: cache_creation_tokens,
        cache_exclusive: cur.is_some_and(|u| u.cache_exclusive),
    };

    let cache_inclusive = is_usage_cache_inclusive(model_name, &usage);

    calculate_token_cost(model_name, &usage, cache_inclusive, cw.has_total_input)
}

/// Format a USD amount according to tiered decimal rules (`$1.23`, `$0.450`, `$0.0123`).
pub fn format_usd(amount: f64) -> String {
    if !amount.is_finite() || amount <= 0.0 {
        return "$0.00".to_string();
    }
    if amount >= 1.0 {
        format!("${amount:.2}")
    } else if amount >= 0.1 {
        format!("${amount:.3}")
    } else {
        format!("${amount:.4}")
    }
}
