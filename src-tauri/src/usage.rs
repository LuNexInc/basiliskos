use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    /// Provider-reported end of this quota window. This is intentionally
    /// separate from the OAuth credential expiry shown on the account.
    pub resets_at_ms: Option<i64>,
    /// False when the provider's billing config is real (proving the account
    /// isn't broken/unreachable) but reported no usage figure at all for this
    /// window — e.g. xAI omits usage fields entirely once an account has
    /// recorded zero usage in the current period. Distinct from a genuine
    /// 0%-used reading so the UI doesn't claim a number it can't back up.
    pub known: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountUsage {
    pub file_name: String,
    pub provider: String,
    pub windows: Vec<GatewayUsageWindow>,
}

pub(crate) fn number_at(value: &Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_f64()
        .or_else(|| current.as_str()?.parse::<f64>().ok())
}

pub(crate) fn usage_window(label: &str, used_percent: f64) -> GatewayUsageWindow {
    let used_percent = used_percent.clamp(0.0, 100.0);
    GatewayUsageWindow {
        label: label.into(),
        used_percent,
        remaining_percent: 100.0 - used_percent,
        resets_at_ms: None,
        known: true,
    }
}

pub(crate) fn usage_window_with_reset(
    label: &str,
    used_percent: f64,
    resets_at_ms: Option<i64>,
) -> GatewayUsageWindow {
    GatewayUsageWindow {
        resets_at_ms,
        ..usage_window(label, used_percent)
    }
}

// Distinct from `usage_window("Week", 0.0)`: this means the provider never
// reported a usage figure at all, not that it reported exactly zero.
pub(crate) fn unrecorded_usage_window(label: &str) -> GatewayUsageWindow {
    GatewayUsageWindow {
        label: label.into(),
        used_percent: 0.0,
        remaining_percent: 100.0,
        resets_at_ms: None,
        known: false,
    }
}

pub(crate) fn unrecorded_usage_window_with_reset(
    label: &str,
    resets_at_ms: Option<i64>,
) -> GatewayUsageWindow {
    GatewayUsageWindow {
        resets_at_ms,
        ..unrecorded_usage_window(label)
    }
}

pub(crate) fn parse_claude_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let mut windows = Vec::new();
    if let Some(used) = number_at(value, &["five_hour", "utilization"]) {
        windows.push(usage_window("5h", used));
    }
    if let Some(used) = number_at(value, &["seven_day", "utilization"]) {
        windows.push(usage_window("Week", used));
    }
    windows
}

pub(crate) fn codex_window_label(window: &Value, fallback: &str) -> String {
    match number_at(window, &["limit_window_seconds"]).map(|value| value as i64) {
        Some(seconds) if (14_400..=21_600).contains(&seconds) => "5h".into(),
        Some(seconds) if (518_400..=691_200).contains(&seconds) => "Week".into(),
        _ => fallback.into(),
    }
}

pub(crate) fn codex_window_reset_ms(window: &Value) -> Option<i64> {
    number_at(window, &["reset_at"])
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * 1000.0) as i64)
}

pub(crate) fn parse_codex_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let mut windows = Vec::new();
    let Some(rate_limit) = value.get("rate_limit") else {
        return windows;
    };
    for (key, fallback) in [("primary_window", "5h"), ("secondary_window", "Week")] {
        let Some(window) = rate_limit.get(key) else {
            continue;
        };
        if let Some(used) = number_at(window, &["used_percent"]) {
            windows.push(usage_window_with_reset(
                &codex_window_label(window, fallback),
                used,
                codex_window_reset_ms(window),
            ));
        }
    }
    windows
}

pub(crate) fn parse_xai_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let resets_at_ms = value
        .pointer("/config/currentPeriod/end")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/config/billingPeriodEnd")
                .and_then(Value::as_str)
        })
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis());
    let product_usage = value
        .get("config")
        .and_then(|config| config.get("productUsage"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                let is_grok_build = item
                    .get("product")
                    .and_then(Value::as_str)
                    .is_none_or(|product| product.eq_ignore_ascii_case("GrokBuild"));
                is_grok_build
                    .then(|| number_at(item, &["usagePercent"]))
                    .flatten()
            })
        });
    // The billing endpoint can report a combined GrokBuild + GrokChat total.
    // Basiliskos routes GrokBuild, so prefer its product-specific percentage.
    if let Some(used) = product_usage
        .or_else(|| number_at(value, &["config", "creditUsagePercent"]))
        .or_else(|| number_at(value, &["creditUsagePercent"]))
    {
        return vec![usage_window_with_reset("Week", used, resets_at_ms)];
    }
    // xAI omits every usage field once an account has recorded zero usage in
    // the current billing period, which is indistinguishable at this point
    // from a response that's missing usage data for some other reason. A
    // present `currentPeriod` proves the billing config itself is real (the
    // account isn't broken/unreachable), so treat that as "hasn't used
    // anything yet" rather than folding it into the same error as a
    // genuinely missing/malformed response.
    let has_real_billing_config = value
        .get("config")
        .and_then(|config| config.get("currentPeriod"))
        .is_some();
    if has_real_billing_config {
        vec![unrecorded_usage_window_with_reset("Week", resets_at_ms)]
    } else {
        Vec::new()
    }
}

pub(crate) fn kimi_usage_percent(value: &Value) -> Option<f64> {
    let limit = number_at(value, &["limit"])?;
    if limit <= 0.0 {
        return None;
    }
    let used = number_at(value, &["used"])
        .or_else(|| number_at(value, &["remaining"]).map(|remaining| limit - remaining))?;
    Some(used / limit * 100.0)
}

pub(crate) fn kimi_usage_label(item: &Value, detail: &Value, index: usize) -> String {
    for value in [item, detail] {
        for key in ["name", "title", "scope"] {
            if let Some(label) = value
                .get(key)
                .and_then(Value::as_str)
                .filter(|label| !label.is_empty())
            {
                return label.into();
            }
        }
    }

    let window = item.get("window").unwrap_or(item);
    let duration = number_at(window, &["duration"])
        .or_else(|| number_at(item, &["duration"]))
        .or_else(|| number_at(detail, &["duration"]));
    let time_unit = window
        .get("timeUnit")
        .or_else(|| item.get("timeUnit"))
        .or_else(|| detail.get("timeUnit"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(duration) = duration {
        let duration = duration as i64;
        if time_unit.contains("MINUTE") {
            return if duration >= 60 && duration % 60 == 0 {
                format!("{}h", duration / 60)
            } else {
                format!("{duration}m")
            };
        }
        if time_unit.contains("HOUR") {
            return format!("{duration}h");
        }
        if time_unit.contains("DAY") {
            return if duration == 7 {
                "Week".into()
            } else {
                format!("{duration}d")
            };
        }
    }
    format!("Limit #{}", index + 1)
}

pub(crate) fn parse_kimi_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let mut windows = Vec::new();
    if let Some(summary) = value.get("usage") {
        if let Some(used) = kimi_usage_percent(summary) {
            let label = summary
                .get("name")
                .or_else(|| summary.get("title"))
                .and_then(Value::as_str)
                .filter(|label| !label.is_empty())
                .unwrap_or("Plan");
            windows.push(usage_window(label, used));
        }
    }
    if let Some(limits) = value.get("limits").and_then(Value::as_array) {
        for (index, item) in limits.iter().enumerate() {
            let detail = item
                .get("detail")
                .filter(|detail| detail.is_object())
                .unwrap_or(item);
            if let Some(used) = kimi_usage_percent(detail) {
                windows.push(usage_window(&kimi_usage_label(item, detail, index), used));
            }
        }
    }
    windows
}
