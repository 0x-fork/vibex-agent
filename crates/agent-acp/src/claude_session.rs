//! Claude Code per-session model routing.
//!
//! Claude Code applies the `env` block of `~/.claude/settings.json` *after* the
//! subprocess environment, so a Provider Profile's projected environment is
//! silently ignored for every key the user's own Claude Code settings define —
//! the model catalog included. `claude-agent-acp` forwards
//! `session/new | session/load | session/resume` `_meta.claudeCode.options` to
//! the Claude Agent SDK, and that programmatic settings tier is applied last,
//! so the Profile can win without editing any user file.
//!
//! The adapter resolves an ACP model value against the picker rows Claude Code
//! derives from `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` and rejects a
//! value that matches no row (`Invalid value for config option model`), which
//! surfaces as a failed model probe and, on the live path, as
//! `runtime_switch_configuration_unavailable` from `apply_session_config`.
//! Every configured model therefore has to own a row: this module assigns each
//! one to a distinct alias slot and projects that mapping into the session
//! settings tier. The alias is only a slot — Claude Code expands it locally, so
//! a slot may point at any model (the user's own settings routinely point
//! `sonnet` at an Opus model).
//!
//! `ANTHROPIC_MODEL` is deliberately not projected when it names an assigned
//! model: it would add a picker row spelling the raw id, and that row wins the
//! adapter's exact-match tier, so `setModel` would receive the raw id and ask
//! the provider to confirm a model spelling it need not recognize instead of
//! the alias Claude Code expands.
//!
//! The `_meta.claudeCode.options` channel exists in `claude-agent-acp` 0.71.0
//! (Vibex's pinned compatibility floor) and later; it is an adapter extension,
//! not standard ACP, so it is only ever built for the `claude` Agent.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use vibex_core::{AcpProviderConfig, AcpProviderEnvSource, AgentId};

use crate::registry::CLAUDE_AGENT_ID;

/// Alias slots Claude Code resolves natively, in assignment order.
const CLAUDE_MODEL_ALIAS_SLOTS: [&str; 3] = ["opus", "sonnet", "haiku"];

/// The `_meta` namespace the Claude adapter reads its SDK options from.
const CLAUDE_SESSION_OPTIONS_NAMESPACE: &str = "claudeCode";

/// Only the model-routing surface is projected. Every other Profile key keeps
/// its existing subprocess-environment path so this change cannot silently
/// change proxy, timeout, or telemetry behavior.
const CLAUDE_MODEL_ROUTING_ENV_PREFIX: &str = "ANTHROPIC_";

const CLAUDE_DEFAULT_MODEL_KEY: &str = "ANTHROPIC_MODEL";

/// Builds the `_meta` value attached to a Claude ACP session request.
///
/// `None` means "send no `_meta` at all": the Agent is not Claude, or the
/// Profile declares nothing this tier can carry.
pub(crate) fn session_settings_meta(
    agent_id: &AgentId,
    config: &AcpProviderConfig,
) -> Option<Value> {
    if agent_id.as_str() != CLAUDE_AGENT_ID {
        return None;
    }
    let assignments = model_slot_assignments(&config.models);
    let mut env = Map::new();
    for reference in &config.env {
        // A secret reference is materialized at spawn time and must never be
        // copied into a request payload; only literals are safe here.
        if reference.source != AcpProviderEnvSource::Literal {
            continue;
        }
        let key = reference.key.trim();
        if !key.starts_with(CLAUDE_MODEL_ROUTING_ENV_PREFIX) {
            continue;
        }
        let Some(value) = reference
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if key == CLAUDE_DEFAULT_MODEL_KEY && assignments.values().any(|model| model == value) {
            continue;
        }
        env.insert(key.to_string(), Value::String(value.to_string()));
    }
    for (slot, model) in &assignments {
        env.insert(
            format!(
                "{CLAUDE_MODEL_ROUTING_ENV_PREFIX}DEFAULT_{}_MODEL",
                slot.to_ascii_uppercase()
            ),
            Value::String(model.clone()),
        );
    }
    if env.is_empty() {
        return None;
    }
    Some(json!({
        CLAUDE_SESSION_OPTIONS_NAMESPACE: {
            "options": {
                "settings": {
                    "env": Value::Object(env),
                }
            }
        }
    }))
}

/// The alias slot each configured model claims, empty for any other Agent.
///
/// The same assignment `session_settings_meta` projects into
/// `ANTHROPIC_DEFAULT_*_MODEL`, exposed so the model-id projection can speak
/// the same vocabulary on the wire: Claude Code answers a Model selection with
/// the slot it resolved, not the id it was handed, so a request that names a
/// concrete id looks like a substitution and fails the switch.
pub(crate) fn model_alias_slots(
    agent_id: &AgentId,
    config: &AcpProviderConfig,
) -> BTreeMap<&'static str, String> {
    if agent_id.as_str() != CLAUDE_AGENT_ID {
        return BTreeMap::new();
    }
    model_slot_assignments(&config.models)
}

/// The environment key that decides which endpoint Claude Code talks to.
///
/// This must win over the user's `~/.claude/settings.json`, and the process
/// environment is not enough to make it win: Claude Code applies the `env` block
/// of that file *after* the subprocess environment, so a user who points
/// `ANTHROPIC_BASE_URL` at another proxy silently keeps talking to it. A Vibex
/// session then names a route the other proxy has never heard of, that proxy
/// answers 404, and Claude Code reports "the selected model may not exist".
///
/// The `_meta` settings tier is applied last, so restating the launch's own
/// address there is what actually routes the session through the Provider the
/// selection named.
///
/// Only the address is restated. The credential deliberately stays on the
/// subprocess environment: a Profile's key can be a real secret, and a
/// materialized secret must never be copied into a request payload. That is
/// enough for the case this fixes — the Vibex gateway ignores inbound
/// credentials by design, so a stale user-supplied token cannot misroute it.
const CLAUDE_CONNECTION_ENV_KEY: &str = "ANTHROPIC_BASE_URL";

/// Restates the launch's endpoint on the tier that outranks the user's file.
///
/// `session_settings_meta` can only project what the Profile declares as
/// literals; the address a launch actually resolves can also come from a
/// projection that is materialized after the config is read. Taking it from the
/// materialized overlay means this tier states the endpoint the process really
/// targets, not a guess.
///
/// Nothing is invented: an overlay that names no address contributes nothing,
/// so a Profile that leaves the endpoint to the user keeps doing so.
pub(crate) fn attach_connection_environment(meta: &mut Option<Value>, env: &[(String, String)]) {
    let Some(address) = env.iter().find_map(|(key, value)| {
        (key.trim() == CLAUDE_CONNECTION_ENV_KEY)
            .then(|| value.trim())
            .filter(|value| !value.is_empty())
    }) else {
        return;
    };
    let meta = meta.get_or_insert_with(|| json!({ CLAUDE_SESSION_OPTIONS_NAMESPACE: {} }));
    let Some(namespace) = meta
        .get_mut(CLAUDE_SESSION_OPTIONS_NAMESPACE)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let options = namespace
        .entry("options".to_string())
        .or_insert_with(|| json!({}));
    let Some(options) = options.as_object_mut() else {
        return;
    };
    let settings = options
        .entry("settings".to_string())
        .or_insert_with(|| json!({}));
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    let settings_env = settings
        .entry("env".to_string())
        .or_insert_with(|| json!({}));
    if let Some(settings_env) = settings_env.as_object_mut() {
        settings_env.insert(
            CLAUDE_CONNECTION_ENV_KEY.to_string(),
            Value::String(address.to_string()),
        );
    }
}

/// Attaches a previously built `_meta` value to a session request payload.
pub(crate) fn attach_session_settings_meta(params: &mut Value, meta: Option<&Value>) {
    let Some(meta) = meta else {
        return;
    };
    let Some(params) = params.as_object_mut() else {
        return;
    };
    params.insert("_meta".to_string(), meta.clone());
}

/// Assigns configured models to distinct Claude Code alias slots.
///
/// A model claims its own family slot when that slot is still free, and the
/// first free slot otherwise; models past the last free slot keep no row and
/// stay unresolvable through this tier. Configured order is the tie-breaker so
/// the mapping is stable across sessions and matches the model list the Profile
/// exposes.
fn model_slot_assignments(models: &[String]) -> BTreeMap<&'static str, String> {
    let mut assignments = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model) {
            continue;
        }
        let slot = model_family(model)
            .filter(|slot| !assignments.contains_key(slot))
            .or_else(|| {
                CLAUDE_MODEL_ALIAS_SLOTS
                    .iter()
                    .copied()
                    .find(|slot| !assignments.contains_key(slot))
            });
        let Some(slot) = slot else {
            break;
        };
        assignments.insert(slot, model.to_string());
    }
    assignments
}

/// The alias family a configured model names, if its id names one.
///
/// Mirrors the alias detection the legacy Provider projection uses to derive a
/// Claude Agent model id, so both layers read `claude-opus-5-5[1m]` as `opus`.
fn model_family(model_id: &str) -> Option<&'static str> {
    model_id
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .find_map(|part| {
            CLAUDE_MODEL_ALIAS_SLOTS
                .iter()
                .copied()
                .find(|slot| *slot == part)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::AcpProviderEnvReference;

    fn literal(key: &str, value: &str) -> AcpProviderEnvReference {
        AcpProviderEnvReference {
            key: key.to_string(),
            source: AcpProviderEnvSource::Literal,
            value: Some(value.to_string()),
            secret_lookup_key: None,
            redacted_hint: "test literal".to_string(),
        }
    }

    fn config(env: Vec<AcpProviderEnvReference>, models: &[&str]) -> AcpProviderConfig {
        AcpProviderConfig {
            command: "claude-agent-acp".to_string(),
            args: Vec::new(),
            env,
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: Default::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: models.iter().map(|model| model.to_string()).collect(),
            modes: Vec::new(),
            features: Vec::new(),
            disabled_tools: Vec::new(),
        }
    }

    fn claude() -> AgentId {
        AgentId::parse(CLAUDE_AGENT_ID).unwrap()
    }

    fn projected_env(meta: &Value) -> Map<String, Value> {
        meta["claudeCode"]["options"]["settings"]["env"]
            .as_object()
            .cloned()
            .expect("session settings carry an env object")
    }

    #[test]
    fn assigns_configured_models_to_distinct_alias_slots() {
        // Two models of the same family cannot share the `opus` row, so the
        // second one takes the next free slot instead of losing its row.
        let meta = session_settings_meta(
            &claude(),
            &config(Vec::new(), &["claude-opus-5[1m]", "claude-opus-5-5[1m]"]),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            Value::String("claude-opus-5-5[1m]".to_string())
        );
        assert!(!env.contains_key("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    }

    #[test]
    fn prefers_the_family_slot_before_overflowing() {
        let meta = session_settings_meta(
            &claude(),
            &config(Vec::new(), &["claude-haiku-4-5", "claude-opus-5[1m]"]),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
            Value::String("claude-haiku-4-5".to_string())
        );
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
    }

    #[test]
    fn models_without_a_family_claim_the_first_free_slot() {
        let meta = session_settings_meta(&claude(), &config(Vec::new(), &["claude-fable-5-1[1m]"]))
            .expect("configured models are projected");

        assert_eq!(
            projected_env(&meta)["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-fable-5-1[1m]".to_string())
        );
    }

    #[test]
    fn drops_the_default_model_key_when_it_would_shadow_an_alias_row() {
        let meta = session_settings_meta(
            &claude(),
            &config(
                vec![
                    literal("ANTHROPIC_MODEL", "claude-opus-5[1m]"),
                    literal("ANTHROPIC_DEFAULT_SONNET_MODEL", "claude-fable-5[1M]"),
                ],
                &["claude-opus-5[1m]"],
            ),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert!(!env.contains_key("ANTHROPIC_MODEL"));
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
        // A Profile alias that no configured model claims stays authoritative.
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            Value::String("claude-fable-5[1M]".to_string())
        );
    }

    #[test]
    fn keeps_the_default_model_key_for_an_unconfigured_model() {
        let meta = session_settings_meta(
            &claude(),
            &config(vec![literal("ANTHROPIC_MODEL", "claude-sonnet-4-5")], &[]),
        )
        .expect("the profile env is projected");

        assert_eq!(
            projected_env(&meta)["ANTHROPIC_MODEL"],
            Value::String("claude-sonnet-4-5".to_string())
        );
    }

    #[test]
    fn ignores_secrets_and_non_model_routing_keys() {
        let secret = AcpProviderEnvReference {
            key: "ANTHROPIC_AUTH_TOKEN".to_string(),
            source: AcpProviderEnvSource::SecretReference,
            value: None,
            secret_lookup_key: Some("vibex-provider-secret".to_string()),
            redacted_hint: "stored in Vibex OS keychain".to_string(),
        };
        let meta = session_settings_meta(
            &claude(),
            &config(
                vec![
                    secret,
                    literal("ANTHROPIC_BASE_URL", "https://gateway.invalid"),
                    literal("HTTP_PROXY", "http://127.0.0.1:7890"),
                ],
                &[],
            ),
        )
        .expect("the profile env is projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_BASE_URL"],
            Value::String("https://gateway.invalid".to_string())
        );
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!env.contains_key("HTTP_PROXY"));
    }

    #[test]
    fn other_agents_and_empty_profiles_send_no_meta() {
        let codex = AgentId::parse("codex").unwrap();
        assert!(
            session_settings_meta(
                &codex,
                &config(vec![literal("ANTHROPIC_MODEL", "m")], &["m"])
            )
            .is_none()
        );
        assert!(session_settings_meta(&claude(), &config(Vec::new(), &[])).is_none());
    }

    /// The materialized endpoint and credential outrank the user's own file.
    #[test]
    fn connection_environment_lands_on_the_settings_tier() {
        let mut meta = session_settings_meta(&claude(), &config(Vec::new(), &["m"]));
        attach_connection_environment(
            &mut meta,
            &[
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "http://127.0.0.1:37543".to_string(),
                ),
                (
                    "HTTP_PROXY".to_string(),
                    "http://127.0.0.1:7890".to_string(),
                ),
            ],
        );
        let env = projected_env(meta.as_ref().expect("connection env is projected"));
        assert_eq!(
            env["ANTHROPIC_BASE_URL"],
            Value::String("http://127.0.0.1:37543".to_string())
        );
        // Unrelated process keys stay on the environment, not in a payload.
        assert!(!env.contains_key("HTTP_PROXY"));
    }

    /// A resolved credential is never copied into a request payload.
    ///
    /// A Profile's key can be a real secret, and this module already refuses to
    /// copy one here; the address is the only connection key that needs the
    /// higher-precedence tier.
    #[test]
    fn connection_environment_never_carries_a_credential() {
        let mut meta = session_settings_meta(&claude(), &config(Vec::new(), &["m"]));
        attach_connection_environment(
            &mut meta,
            &[
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "http://127.0.0.1:37543".to_string(),
                ),
                (
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "resolved-secret".to_string(),
                ),
                ("ANTHROPIC_API_KEY".to_string(), "resolved-key".to_string()),
            ],
        );
        let env = projected_env(meta.as_ref().expect("connection env is projected"));
        assert!(env.get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(env.get("ANTHROPIC_API_KEY").is_none());
        let rendered = format!("{meta:?}");
        assert!(!rendered.contains("resolved-secret"));
        assert!(!rendered.contains("resolved-key"));
    }

    /// A Profile that sets no endpoint keeps letting the user supply one.
    #[test]
    fn connection_environment_invents_nothing() {
        let mut meta = session_settings_meta(&claude(), &config(Vec::new(), &["m"]));
        let before = meta.clone();
        attach_connection_environment(&mut meta, &[]);
        attach_connection_environment(
            &mut meta,
            &[(
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:7890".to_string(),
            )],
        );
        assert_eq!(meta, before);
    }

    #[test]
    fn attach_meta_leaves_payloads_without_settings_untouched() {
        let mut params = json!({ "cwd": "/tmp/workspace", "mcpServers": [] });
        attach_session_settings_meta(&mut params, None);
        assert!(params.get("_meta").is_none());

        let meta = session_settings_meta(&claude(), &config(Vec::new(), &["claude-opus-5[1m]"]))
            .expect("configured models are projected");
        attach_session_settings_meta(&mut params, Some(&meta));
        assert_eq!(params["_meta"], meta);
    }
}
