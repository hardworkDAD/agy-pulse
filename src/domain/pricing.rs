use std::sync::OnceLock;

use regex::Regex;

use super::model::{TokenUsage, lookup_known_display_name};

/// Per-million-token pricing rates in USD, including optional >200k long-context tiers.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub long_threshold: Option<u64>,
    pub long_input: Option<f64>,
    pub long_output: Option<f64>,
    pub long_cache_read: Option<f64>,
    pub long_cache_write: Option<f64>,
}

const DEFAULT_PRICING: ModelPricing = ModelPricing {
    input: 0.15,
    output: 0.60,
    cache_read: 0.0375,
    cache_write: 0.15,
    long_threshold: None,
    long_input: None,
    long_output: None,
    long_cache_read: None,
    long_cache_write: None,
};

struct PricingRule {
    regex: Regex,
    pricing: ModelPricing,
}

fn pricing_rules() -> &'static [PricingRule] {
    static RULES: OnceLock<Vec<PricingRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let specs: &[(&str, ModelPricing)] = &[
            (
                r"(?i)opus-?5[.-]?5|opus\s+5\.5|5[.-]5[-_\s]+opus",
                ModelPricing {
                    input: 4.00,
                    output: 20.00,
                    cache_read: 0.20,
                    cache_write: 5.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)sonnet-?5[.-]?5|sonnet\s+5\.5|5[.-]5[-_\s]+sonnet",
                ModelPricing {
                    input: 2.00,
                    output: 10.00,
                    cache_read: 0.10,
                    cache_write: 2.50,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)opus[-_\s]*4[.-]?6|4[.-]6[-_\s]+opus",
                ModelPricing {
                    input: 5.00,
                    output: 25.00,
                    cache_read: 0.50,
                    cache_write: 6.25,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)sonnet[-_\s]*4[.-]?6|4[.-]6[-_\s]+sonnet",
                ModelPricing {
                    input: 3.00,
                    output: 15.00,
                    cache_read: 0.30,
                    cache_write: 3.75,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)haiku[-_\s]*4[.-]?5|4[.-]5[-_\s]+haiku",
                ModelPricing {
                    input: 0.80,
                    output: 4.00,
                    cache_read: 0.08,
                    cache_write: 1.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)opus",
                ModelPricing {
                    input: 15.00,
                    output: 75.00,
                    cache_read: 1.50,
                    cache_write: 18.75,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)sonnet",
                ModelPricing {
                    input: 3.00,
                    output: 15.00,
                    cache_read: 0.30,
                    cache_write: 3.75,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)haiku",
                ModelPricing {
                    input: 0.80,
                    output: 4.00,
                    cache_read: 0.08,
                    cache_write: 1.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)hydrogen|\bhydro\b",
                ModelPricing {
                    input: 1.50,
                    output: 7.50,
                    cache_read: 0.075,
                    cache_write: 1.50,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)carbon|orchestrator.*carbon|\borch-c\b",
                ModelPricing {
                    input: 2.00,
                    output: 10.00,
                    cache_read: 0.10,
                    cache_write: 2.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)argon|orchestrator.*argon|\borch-a\b",
                ModelPricing {
                    input: 2.00,
                    output: 10.00,
                    cache_read: 0.10,
                    cache_write: 2.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)neon|orchestrator.*neon|\borch-n\b",
                ModelPricing {
                    input: 3.00,
                    output: 15.00,
                    cache_read: 0.15,
                    cache_write: 3.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)orchestrator",
                ModelPricing {
                    input: 2.00,
                    output: 10.00,
                    cache_read: 0.10,
                    cache_write: 2.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)gemini[-_\s]*3(?:\.\d+)?[-_\s]*flash",
                ModelPricing {
                    input: 0.75,
                    output: 3.75,
                    cache_read: 0.075,
                    cache_write: 0.75,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)flash",
                ModelPricing {
                    input: 0.15,
                    output: 0.60,
                    cache_read: 0.0375,
                    cache_write: 0.15,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)deepseek",
                ModelPricing {
                    input: 0.27,
                    output: 1.10,
                    cache_read: 0.07,
                    cache_write: 0.27,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)\bo1-mini",
                ModelPricing {
                    input: 1.10,
                    output: 4.40,
                    cache_read: 0.55,
                    cache_write: 1.10,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)\bo1-",
                ModelPricing {
                    input: 15.00,
                    output: 60.00,
                    cache_read: 7.50,
                    cache_write: 15.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)\bo3-mini",
                ModelPricing {
                    input: 1.10,
                    output: 4.40,
                    cache_read: 0.55,
                    cache_write: 1.10,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)\bo3-",
                ModelPricing {
                    input: 2.00,
                    output: 8.00,
                    cache_read: 0.50,
                    cache_write: 2.00,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)\bo4-",
                ModelPricing {
                    input: 1.10,
                    output: 4.40,
                    cache_read: 0.275,
                    cache_write: 1.10,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)gemini[-_\s]*(?:3\.1[-_\s]*pro|pro[-_\s]*agent)",
                ModelPricing {
                    input: 2.00,
                    output: 12.00,
                    cache_read: 0.20,
                    cache_write: 2.00,
                    long_threshold: Some(200_000),
                    long_input: Some(4.00),
                    long_output: Some(18.00),
                    long_cache_read: Some(0.40),
                    long_cache_write: Some(4.00),
                },
            ),
            (
                r"(?i)pro",
                ModelPricing {
                    input: 1.25,
                    output: 10.00,
                    cache_read: 0.125,
                    cache_write: 1.25,
                    long_threshold: Some(200_000),
                    long_input: Some(2.50),
                    long_output: Some(15.00),
                    long_cache_read: Some(0.25),
                    long_cache_write: Some(2.50),
                },
            ),
            (
                r"(?i)gpt-oss",
                ModelPricing {
                    input: 0.15,
                    output: 0.60,
                    cache_read: 0.075,
                    cache_write: 0.15,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
            (
                r"(?i)gpt-?4",
                ModelPricing {
                    input: 2.50,
                    output: 10.00,
                    cache_read: 1.25,
                    cache_write: 2.50,
                    long_threshold: None,
                    long_input: None,
                    long_output: None,
                    long_cache_read: None,
                    long_cache_write: None,
                },
            ),
        ];

        specs
            .iter()
            .filter_map(|(pat, pricing)| {
                Regex::new(pat).ok().map(|regex| PricingRule {
                    regex,
                    pricing: *pricing,
                })
            })
            .collect()
    })
}

/// Look up pricing for a model name or ID, falling back to `DEFAULT_PRICING`.
pub fn lookup_pricing(model_name_or_id: &str) -> ModelPricing {
    let trimmed = model_name_or_id.trim();
    let canonical = lookup_known_display_name(trimmed).unwrap_or(trimmed);
    for rule in pricing_rules() {
        if rule.regex.is_match(canonical) {
            return rule.pricing;
        }
    }
    if canonical != trimmed {
        for rule in pricing_rules() {
            if rule.regex.is_match(trimmed) {
                return rule.pricing;
            }
        }
    }
    DEFAULT_PRICING
}

/// Calculate estimated USD cost from token counts and model pricing.
pub fn calculate_token_cost(
    model_name_or_id: &str,
    tokens: &TokenUsage,
    cache_inclusive: bool,
    is_cumulative: bool,
) -> Option<f64> {
    let pricing = lookup_pricing(model_name_or_id);
    let cache_total = tokens
        .cache_read_input_tokens
        .saturating_add(tokens.cache_creation_input_tokens);
    let is_actually_inclusive = cache_inclusive && tokens.input_tokens >= cache_total;
    let in_tokens = if is_actually_inclusive {
        tokens.input_tokens - cache_total
    } else {
        tokens.input_tokens
    };

    let total_prompt_tokens = in_tokens.saturating_add(cache_total);
    let use_long_tier = !is_cumulative
        && pricing
            .long_threshold
            .is_some_and(|threshold| total_prompt_tokens > threshold);

    let input_rate = if use_long_tier {
        pricing.long_input.unwrap_or(pricing.input)
    } else {
        pricing.input
    };
    let output_rate = if use_long_tier {
        pricing.long_output.unwrap_or(pricing.output)
    } else {
        pricing.output
    };
    let cache_read_rate = if use_long_tier {
        pricing.long_cache_read.unwrap_or(pricing.cache_read)
    } else {
        pricing.cache_read
    };
    let cache_write_rate = if use_long_tier {
        pricing.long_cache_write.unwrap_or(pricing.cache_write)
    } else {
        pricing.cache_write
    };

    let cost = (in_tokens as f64 / 1_000_000.0) * input_rate
        + (tokens.output_tokens as f64 / 1_000_000.0) * output_rate
        + (tokens.cache_read_input_tokens as f64 / 1_000_000.0) * cache_read_rate
        + (tokens.cache_creation_input_tokens as f64 / 1_000_000.0) * cache_write_rate;

    if cost.is_finite() && cost >= 0.0 {
        Some(cost)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_pro_agent_matches_gemini_3_1_pro_rates() {
        let by_id = lookup_pricing("gemini-pro-agent");
        let by_name = lookup_pricing("Gemini 3.1 Pro (High)");
        assert!((by_id.input - by_name.input).abs() < 1e-12);
        assert!((by_id.output - by_name.output).abs() < 1e-12);
        assert!((by_id.cache_read - by_name.cache_read).abs() < 1e-12);
        assert!((by_id.cache_write - by_name.cache_write).abs() < 1e-12);
        assert_eq!(by_id.long_threshold, by_name.long_threshold);
    }

    #[test]
    fn all_mapped_models_match_display_name_pricing() {
        for &(id, display_name) in crate::domain::model::MODEL_DISPLAY_NAMES {
            let p_id = lookup_pricing(id);
            let p_upper = lookup_pricing(&id.to_ascii_uppercase());
            let p_name = lookup_pricing(display_name);
            assert!(
                (p_id.input - p_name.input).abs() < 1e-12
                    && (p_id.output - p_name.output).abs() < 1e-12
                    && (p_id.cache_read - p_name.cache_read).abs() < 1e-12
                    && (p_id.cache_write - p_name.cache_write).abs() < 1e-12
                    && p_id.long_threshold == p_name.long_threshold
                    && p_id.long_input == p_name.long_input
                    && p_id.long_output == p_name.long_output
                    && p_id.long_cache_read == p_name.long_cache_read
                    && p_id.long_cache_write == p_name.long_cache_write,
                "pricing mismatch for mapped model id={id:?} vs display={display_name:?}"
            );
            assert!(
                (p_upper.input - p_name.input).abs() < 1e-12
                    && (p_upper.output - p_name.output).abs() < 1e-12,
                "case-insensitive pricing mismatch for id={id:?}"
            );
        }
    }
}
