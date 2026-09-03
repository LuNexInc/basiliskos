use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gateway::RouteSelection;

// Supported OAuth-capable providers, in UI order. Adding a provider here
// changes the account tabs, route defaults, and the config generation surface.
pub(crate) const SUPPORTED_PROVIDERS: [&str; 6] =
    ["claude", "codex", "xai", "kimi", "antigravity", "zai"];

/// How an account authenticates. An OAuth-capable provider can also be reached
/// with a static API key; an API-key-only provider (DeepSeek, routers, custom
/// endpoints) can never use the browser OAuth flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderAuth {
    Oauth,
    ApiKey,
}

/// Providers reached only with a static API key + endpoint (never OAuth).
/// OpenCodex was dropped in 3.0; these replace the catalog scaffold with a
/// first-class API-key slot.
pub(crate) const API_KEY_PROVIDERS: &[&str] =
    &["deepseek", "opencode", "openrouter", "litellm", "custom"];

/// Every provider that can name a credential, for account detection. OAuth
/// providers come first so filename-prefix matching stays unambiguous.
pub(crate) fn all_providers() -> Vec<&'static str> {
    let mut providers = SUPPORTED_PROVIDERS.to_vec();
    providers.extend_from_slice(API_KEY_PROVIDERS);
    providers
}

/// The auth methods a provider supports. OAuth providers also accept a key, so
/// Claude / Codex / Grok / Antigravity / Kimi / Z.AI can each be used with a BYO key.
pub(crate) fn auth_kinds_for(provider: &str) -> &'static [ProviderAuth] {
    if API_KEY_PROVIDERS.contains(&provider) {
        &[ProviderAuth::ApiKey]
    } else {
        &[ProviderAuth::Oauth, ProviderAuth::ApiKey]
    }
}

/// Default upstream base URL for API-key mode. Lane routers with no fixed
/// endpoint (LiteLLM, custom) return None so the user supplies one.
pub(crate) fn default_api_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some("https://api.anthropic.com"),
        "codex" => Some("https://api.openai.com"),
        "xai" => Some("https://api.x.ai"),
        "antigravity" => Some("https://generativelanguage.googleapis.com"),
        "kimi" => Some("https://api.moonshot.ai"),
        "zai" => Some("https://api.z.ai/api/coding/paas/v4"),
        "deepseek" => Some("https://api.deepseek.com"),
        "opencode" => Some("https://opencode.ai"),
        "openrouter" => Some("https://openrouter.ai/api"),
        // Self-hosted proxy: the user supplies the base URL.
        "litellm" | "custom" => None,
        _ => None,
    }
}

pub(crate) const GROK_4_5_CONTEXT_WINDOW_TOKENS: u64 = 500_000;

#[derive(Clone, Copy)]
pub(crate) struct ModelSpec {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) thinking_levels: &'static [&'static str],
}

pub(crate) const CLAUDE_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-sonnet-4-5-20250929",
        label: "Claude Sonnet 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-5-20251101",
        label: "Claude Opus 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-haiku-4-5-20251001",
        label: "Claude Haiku 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
];

pub(crate) const CODEX_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        thinking_levels: &["low", "medium", "high", "xhigh", "max", "ultra"],
    },
    ModelSpec {
        id: "gpt-5.6-terra",
        label: "GPT-5.6 Terra",
        thinking_levels: &["low", "medium", "high", "xhigh", "max", "ultra"],
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        thinking_levels: &["low", "medium", "high", "xhigh", "max"],
    },
];

pub(crate) const XAI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "grok-4.6",
        label: "Grok 4.6",
        // 7.2.131 registry: low/medium/high/xhigh; 500k context.
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
    ModelSpec {
        id: "grok-4.5",
        label: "Grok 4.5",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-composer-2.5-fast",
        label: "Grok Composer 2.5 Fast",
        thinking_levels: &[],
    },
];

pub(crate) const KIMI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "kimi-k3",
        label: "Kimi K3",
        // K3 always thinks. CLIProxyAPI exposes low, high, and max.
        thinking_levels: &["low", "high", "max"],
    },
    ModelSpec {
        id: "kimi-k2.7-code",
        label: "Kimi K2.7 Code",
        thinking_levels: &["low", "high"],
    },
];

pub(crate) const ANTIGRAVITY_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-3.8-flash",
        label: "Gemini 3.8 Flash",
        thinking_levels: &["low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "gemini-3.7-flash",
        label: "Gemini 3.7 Flash",
        thinking_levels: &["low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "gemini-3.7-pro",
        label: "Gemini 3.7 Pro",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "gemini-3.7-flash-lite",
        label: "Gemini 3.7 Flash Lite",
        thinking_levels: &["none", "low", "high"],
    },
];

pub(crate) const ZAI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "glm-5.3",
        label: "GLM-5.3",
        // Thinking is not mapped through the openai-compatibility hop yet.
        thinking_levels: &[],
    },
    ModelSpec {
        id: "glm-5.3-flash",
        label: "GLM-5.3 Flash",
        thinking_levels: &[],
    },
];

// API-key providers have a known model set (DeepSeek) or a live catalog fetched
// from the backend. Routers / custom endpoints return empty pinned specs here;
// their picker options come from `refresh_model_catalog_cache`'s live list.
pub(crate) const DEEPSEEK_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "deepseek-chat",
        label: "DeepSeek Chat",
        thinking_levels: &["none"],
    },
    ModelSpec {
        id: "deepseek-reasoner",
        label: "DeepSeek Reasoner",
        thinking_levels: &["none", "high"],
    },
];

pub(crate) fn model_specs(provider: &str) -> &'static [ModelSpec] {
    match provider {
        "claude" => CLAUDE_MODELS,
        "codex" => CODEX_MODELS,
        "xai" => XAI_MODELS,
        "kimi" => KIMI_MODELS,
        "antigravity" => ANTIGRAVITY_MODELS,
        "zai" => ZAI_MODELS,
        "deepseek" => DEEPSEEK_MODELS,
        // Routers and custom endpoints expose a live model catalog; no pinned
        // specs until the alias/live-catalog feature lands.
        "opencode" | "openrouter" | "litellm" | "custom" => &[],
        _ => &[],
    }
}

pub(crate) fn default_model(provider: &str) -> &'static str {
    match provider {
        "claude" => "claude-sonnet-4-5-20250929",
        "codex" => "gpt-5.6-terra",
        "xai" => "grok-4.5",
        "kimi" => "kimi-k3",
        "antigravity" => "gemini-3.8-flash",
        "zai" => "glm-5.3",
        "deepseek" => "deepseek-chat",
        // Routers/custom: the live catalog validates and refines the actual
        // model id; a placeholder keeps the route non-empty until fetched.
        "opencode" => "opencode-go",
        "openrouter" => "auto",
        "litellm" => "auto",
        "custom" => "auto",
        _ => "",
    }
}

pub(crate) fn default_routes() -> BTreeMap<String, RouteSelection> {
    all_providers()
        .into_iter()
        .map(|provider| {
            (
                provider.to_string(),
                RouteSelection {
                    model: default_model(provider).to_string(),
                    thinking: "auto".into(),
                },
            )
        })
        .collect()
}

pub(crate) fn context_window_for_route(provider: &str, model: &str) -> Option<u64> {
    match (provider, model) {
        ("xai", "grok-4.5" | "grok-4.6") => Some(GROK_4_5_CONTEXT_WINDOW_TOKENS),
        ("zai", "glm-5.3" | "glm-5.3-flash") => Some(1_048_576),
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ContextBudget {
    pub(crate) window_tokens: u64,
    pub(crate) reserved_output_tokens: u64,
}

pub(crate) fn context_budget_for_request(provider: &str, request: &Value) -> Option<ContextBudget> {
    let model = request.get("model")?.as_str()?;
    if provider != "xai" || !(model.starts_with("grok-4.5") || model.starts_with("grok-4.6")) {
        return None;
    }
    Some(ContextBudget {
        window_tokens: GROK_4_5_CONTEXT_WINDOW_TOKENS,
        reserved_output_tokens: request
            .get("max_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Claude Desktop validates `inferenceModels[].name` against Anthropic's own
/// provider catalog even with `unstableDisableModelVerification` — the name
/// must be a real Anthropic model id (verified empirically 2026-08-12 against
/// Claude 1.28929: `deepseek-v4-flash` → invalid; `claude-sonnet-4-5` etc. →
/// healthy). So every advertised picker entry routes through a stable
/// Anthropic alias and the front proxy maps the alias back to the real
/// upstream model (see gateway.rs `client_model_choice`). Aliases are unique
/// within a provider so Claude's picker can distinguish entries. The pool below
/// covers the largest supported picker plus its selected-model thinking variants.
const ANTHROPIC_ALIAS_POOL: [&str; 17] = [
    "claude-sonnet-4-5",
    "claude-opus-4-5",
    "claude-haiku-4-5",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-fable-5",
    "claude-sonnet-4-5-20250929",
    "claude-opus-4-5-20251101",
    "claude-haiku-4-5-20251001",
    "claude-sonnet-4-7",
    "claude-opus-4-9",
    "claude-haiku-4-6",
    "claude-sonnet-4-8",
    "claude-opus-4-10",
    "claude-haiku-4-7",
];

/// Stable Anthropic base alias for a provider model. The claude provider
/// aliases to its own model id (already an Anthropic catalog id).
pub(crate) fn base_alias(provider: &str, model: &str) -> Option<&'static str> {
    if provider == "claude" {
        return model_specs("claude")
            .iter()
            .find(|spec| spec.id == model)
            .map(|spec| spec.id);
    }
    let specs = model_specs(provider);
    let index = specs.iter().position(|spec| spec.id == model)?;
    ANTHROPIC_ALIAS_POOL.get(index).copied()
}

/// Human label for a thinking level shown in the picker entries.
pub(crate) fn thinking_level_label(level: &str) -> &'static str {
    match level {
        "auto" => "Auto",
        "none" => "Off",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Maximum",
        "ultra" => "Ultra",
        _ => "Auto",
    }
}

/// The single, deterministic source of truth for a provider's picker aliases.
/// Returns `(alias, model, thinking)` in picker order — the selected model's
/// base alias, its thinking-variant aliases, then every other catalog base
/// alias. Aliases are unique and Anthropic-valid, and the assignment is
/// independent of the hidden list (which only filters display, never slot
/// numbering), so the display (`picker_entries`) and the reverse map
/// (`alias_to_picker_entry`) can never disagree.
fn picker_alias_spec(provider: &str, selected: &str) -> Vec<(String, String, String)> {
    let specs = model_specs(provider);
    let mut out = Vec::new();
    if let Some(selected_spec) = specs.iter().find(|spec| spec.id == selected) {
        if let Some(alias) = base_alias(provider, selected_spec.id) {
            out.push((
                alias.to_string(),
                selected_spec.id.to_string(),
                "auto".into(),
            ));
            // Thinking variants use pool slots after the base models. Only one
            // model's variants are advertised at a time (the selected one), so
            // the slots are unambiguous: index < n_models is a base model,
            // index >= n_models is a thinking variant of the selected model.
            if provider != "claude" && selected_spec.thinking_levels.len() > 1 {
                for (index, level) in selected_spec.thinking_levels.iter().enumerate() {
                    if let Some(alias) = ANTHROPIC_ALIAS_POOL.get(specs.len() + index) {
                        out.push((
                            alias.to_string(),
                            selected_spec.id.to_string(),
                            level.to_string(),
                        ));
                    }
                }
            }
        }
    }
    for spec in specs {
        if spec.id == selected {
            continue;
        }
        if let Some(alias) = base_alias(provider, spec.id) {
            out.push((alias.to_string(), spec.id.to_string(), "auto".into()));
        }
    }
    out
}

/// Builds the Claude picker entries for the active provider: the selected
/// model first (its base entry, then an explicit entry per supported thinking
/// level), then the other visible models as base entries. Returns
/// (alias, label, upstream model, thinking).
pub(crate) fn picker_entries(
    provider: &str,
    hidden: &BTreeSet<String>,
    selected: &str,
) -> Vec<(String, String, String, String)> {
    let specs = model_specs(provider);
    let label_for = |model: &str| {
        specs
            .iter()
            .find(|spec| spec.id == model)
            .map(|spec| spec.label.to_string())
            .unwrap_or_else(|| model.to_string())
    };
    picker_alias_spec(provider, selected)
        .into_iter()
        .filter(|(_, model, _)| model == selected || !hidden.contains(model))
        .map(|(alias, model, thinking)| {
            let label = if thinking == "auto" {
                label_for(&model)
            } else {
                format!(
                    "{} · {}",
                    label_for(&model),
                    thinking_level_label(&thinking)
                )
            };
            (alias, label, model, thinking)
        })
        .collect()
}

/// Reverse lookup for a picker request: Anthropic alias → (upstream model,
/// thinking). Resolved against the same generator as `picker_entries`, so the
/// two directions share one alias assignment.
pub(crate) fn alias_to_picker_entry(
    provider: &str,
    alias: &str,
    selected: &str,
) -> Option<(String, String)> {
    picker_alias_spec(provider, selected)
        .into_iter()
        .find(|(candidate, _, _)| candidate == alias)
        .map(|(_, model, thinking)| (model, thinking))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_providers_are_key_only() {
        for provider in API_KEY_PROVIDERS {
            assert_eq!(auth_kinds_for(provider), &[ProviderAuth::ApiKey]);
        }
    }

    #[test]
    fn oauth_providers_accept_a_key_too() {
        for provider in SUPPORTED_PROVIDERS {
            assert!(auth_kinds_for(provider).contains(&ProviderAuth::Oauth));
            assert!(auth_kinds_for(provider).contains(&ProviderAuth::ApiKey));
        }
    }

    #[test]
    fn deepseek_has_defaults_and_model_specs() {
        assert_eq!(default_model("deepseek"), "deepseek-chat");
        assert_eq!(
            default_api_base_url("deepseek"),
            Some("https://api.deepseek.com")
        );
        assert!(model_specs("deepseek")
            .iter()
            .any(|spec| spec.id == "deepseek-chat"));
    }

    #[test]
    fn routers_and_custom_are_live_catalog() {
        for provider in ["opencode", "openrouter", "litellm", "custom"] {
            assert!(model_specs(provider).is_empty());
        }
        assert_eq!(
            default_api_base_url("opencode"),
            Some("https://opencode.ai")
        );
        assert_eq!(
            default_api_base_url("openrouter"),
            Some("https://openrouter.ai/api")
        );
        assert_eq!(default_api_base_url("litellm"), None);
        assert_eq!(default_api_base_url("custom"), None);
    }

    #[test]
    fn all_providers_covers_every_catalog_entry() {
        let all = all_providers();
        for provider in SUPPORTED_PROVIDERS.iter().chain(API_KEY_PROVIDERS.iter()) {
            assert!(all.contains(provider));
        }
    }

    #[test]
    fn picker_alias_spec_is_stable_and_reversible() {
        for provider in SUPPORTED_PROVIDERS {
            let selected = default_model(provider);
            let spec = picker_alias_spec(provider, selected);
            assert!(!spec.is_empty(), "{provider} alias spec is empty");
            let mut seen = std::collections::BTreeSet::new();
            for (alias, model, _thinking) in &spec {
                assert!(
                    seen.insert(alias.clone()),
                    "{provider} alias {alias} duplicated"
                );
                // Every advertised alias reverses back to the same upstream model.
                assert_eq!(
                    alias_to_picker_entry(provider, alias, selected).map(|(m, _)| m),
                    Some(model.clone()),
                    "{provider} alias {alias} did not reverse to {model}"
                );
            }
            // The hidden list filters display only — it never shifts the alias
            // assignment, so the reverse map stays stable.
            let hidden = std::collections::BTreeSet::from([selected.to_string()]);
            let _ = picker_entries(provider, &hidden, selected);
        }
        // Live-catalog providers (routers/custom) have no pinned aliases yet.
        for provider in ["opencode", "openrouter", "litellm", "custom"] {
            assert!(picker_alias_spec(provider, default_model(provider)).is_empty());
        }
    }
}
