use serde::{Deserialize, Serialize};

use super::quota::ProviderQuotaSummary;

/// Sentinel account identifier used when a conversation or device has not been assigned an email.
pub const UNASSIGNED_ACCOUNT_ID: &str = "unassigned";

/// Normalize a raw account identifier (email) by trimming whitespace and lowercasing ASCII.
/// Returns `None` if the trimmed input is empty.
pub fn normalize_account_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

/// Immutable device identity paired with a user-facing device name and locality flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub is_local: bool,
}

/// Cross-device quota observation snapshot bound to an account and reporting device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub account_id: String,
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub plan_tier: Option<String>,
    pub observed_at_ms: i64,
    #[serde(default)]
    pub quota: ProviderQuotaSummary,
}
