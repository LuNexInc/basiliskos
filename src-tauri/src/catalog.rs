use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::gateway::RouteSelection;

// Supported live providers, in UI order. Adding a provider here changes the
// account tabs, route defaults, and the config generation surface.
pub(crate) const SUPPORTED_PROVIDERS: [&str; 5] = ["claude", "codex", "xai", "kimi", "deepseek"];

pub(crate) const GROK_4_5_CONTEXT_WINDOW_TOKENS: u64 = 500_000;

#[derive(Clone, Copy)]
pub(crate) struct ModelSpec {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) thinking_levels: &'static [&'static str],
}

pub(crate) const CLAUDE_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-sonnet-4-5-20250929",
        label: "Claude Sonnet 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-5-20251101",
        label: "Claude Opus 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-7",
        label: "Claude Opus 4.7",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
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
        id: "gpt-5.5",
        label: "GPT-5.5",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
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
    ModelSpec {
        id: "gpt-5.4",
        label: "GPT-5.4",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
    ModelSpec {
        id: "gpt-5.4-mini",
        label: "GPT-5.4 Mini",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
];

pub(crate) const XAI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "grok-build-0.1",
        label: "Grok Build 0.1",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.5",
        label: "Grok 4.5",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-4.3",
        label: "Grok 4.3",
        thinking_levels: &["none", "low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-4.20-0309-reasoning",
        label: "Grok 4.20 Reasoning",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.20-0309-non-reasoning",
        label: "Grok 4.20 Non-Reasoning",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.20-multi-agent-0309",
        label: "Grok 4.20 Multi-Agent",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-3-mini",
        label: "Grok 3 Mini",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-3-mini-fast",
        label: "Grok 3 Mini Fast",
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
        // K3 always thinks; official API currently only accepts reasoning_effort=max.
        thinking_levels: &["max"],
    },
    ModelSpec {
        id: "kimi-k2.7-code",
        label: "Kimi K2.7 Code",
        thinking_levels: &["low", "high"],
    },
    ModelSpec {
        id: "kimi-k2.7-code-highspeed",
        label: "Kimi K2.7 Code HighSpeed",
        thinking_levels: &["low", "high"],
    },
    ModelSpec {
        id: "kimi-k2.6",
        label: "Kimi K2.6",
        thinking_levels: &["none", "low", "high"],
    },
    ModelSpec {
        id: "kimi-k2.5",
        label: "Kimi K2.5",
        thinking_levels: &["none", "low", "high"],
    },
    ModelSpec {
        id: "kimi-k2-thinking",
        label: "Kimi K2 Thinking",
        thinking_levels: &["none", "low", "high"],
    },
    ModelSpec {
        id: "kimi-k2",
        label: "Kimi K2",
        thinking_levels: &[],
    },
];

// `deepseek-chat` / `deepseek-reasoner` were fully retired on 2026-07-24 and now
// return errors, so V4 is the only routable generation.
//
// Thinking is delivered through Anthropic adaptive thinking, which CLIProxyAPI
// translates to the OpenAI-compatible upstream's `reasoning_effort`. Do not use
// the `model(effort)` suffix used for OAuth providers: it breaks credential
// selection on the openai-compatibility path.
pub(crate) const DEEPSEEK_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "deepseek-v4-flash",
        label: "DeepSeek V4 Flash",
        thinking_levels: &["none", "low", "high", "max"],
    },
    ModelSpec {
        id: "deepseek-v4-pro",
        label: "DeepSeek V4 Pro",
        thinking_levels: &["none", "high", "max"],
    },
];

pub(crate) fn model_specs(provider: &str) -> &'static [ModelSpec] {
    match provider {
        "claude" => CLAUDE_MODELS,
        "codex" => CODEX_MODELS,
        "xai" => XAI_MODELS,
        "kimi" => KIMI_MODELS,
        "deepseek" => DEEPSEEK_MODELS,
        _ => &[],
    }
}

pub(crate) fn default_model(provider: &str) -> &'static str {
    match provider {
        "claude" => "claude-sonnet-4-5-20250929",
        "codex" => "gpt-5.5",
        "xai" => "grok-build-0.1",
        "kimi" => "kimi-k3",
        "deepseek" => "deepseek-v4-flash",
        _ => "",
    }
}

pub(crate) fn default_routes() -> BTreeMap<String, RouteSelection> {
    SUPPORTED_PROVIDERS
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
        ("xai", "grok-4.5") => Some(GROK_4_5_CONTEXT_WINDOW_TOKENS),
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
    if provider != "xai" || !model.starts_with("grok-4.5") {
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
/// covers the largest provider (xai, 9 models).
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
    let visible = |spec: &ModelSpec| spec.id == selected || !hidden.contains(spec.id);
    let mut entries = Vec::new();
    if let Some(spec) = specs.iter().find(|spec| spec.id == selected) {
        if visible(spec) {
            if let Some(alias) = base_alias(provider, spec.id) {
                entries.push((
                    alias.to_string(),
                    spec.label.to_string(),
                    spec.id.to_string(),
                    "auto".into(),
                ));
            }
            // Thinking variants use pool slots after the base models. Only one
            // model's variants are advertised at a time (the selected one), so
            // the slots are unambiguous: index < n_models is a base model,
            // index >= n_models is a thinking variant of the selected model.
            if provider != "claude" && spec.thinking_levels.len() > 1 {
                for (index, level) in spec.thinking_levels.iter().enumerate() {
                    if let Some(alias) = ANTHROPIC_ALIAS_POOL.get(specs.len() + index) {
                        entries.push((
                            alias.to_string(),
                            format!("{} · {}", spec.label, thinking_level_label(level)),
                            spec.id.to_string(),
                            level.to_string(),
                        ));
                    }
                }
            }
        }
    }
    for spec in specs {
        if spec.id == selected || !visible(spec) {
            continue;
        }
        if let Some(alias) = base_alias(provider, spec.id) {
            entries.push((
                alias.to_string(),
                spec.label.to_string(),
                spec.id.to_string(),
                "auto".into(),
            ));
        }
    }
    entries
}

/// Reverse lookup for a picker request: Anthropic alias → (upstream model,
/// thinking). Base aliases resolve from the provider catalog with "auto"
/// thinking; variant aliases (index >= model count) resolve against the
/// selected model's thinking levels.
pub(crate) fn alias_to_picker_entry(
    provider: &str,
    alias: &str,
    selected: &str,
) -> Option<(String, String)> {
    if provider == "claude" {
        return model_specs("claude")
            .iter()
            .find(|spec| spec.id == alias)
            .map(|spec| (spec.id.to_string(), "auto".into()));
    }
    let specs = model_specs(provider);
    let index = ANTHROPIC_ALIAS_POOL
        .iter()
        .position(|candidate| *candidate == alias)?;
    if index < specs.len() {
        return Some((specs[index].id.to_string(), "auto".into()));
    }
    let selected_spec = specs.iter().find(|spec| spec.id == selected)?;
    if selected_spec.thinking_levels.len() > 1 {
        let level = selected_spec.thinking_levels.get(index - specs.len())?;
        return Some((selected.to_string(), (*level).to_string()));
    }
    None
}
