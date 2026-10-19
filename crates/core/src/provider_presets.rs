//! Built-in Provider presets.
//!
//! A preset answers the three questions a user otherwise has to research
//! before adding a Provider: which address to call, which wire protocol that
//! address speaks, and which model ids it offers. The table ships with the
//! binary — it is code, not remotely updated data — because the address and
//! protocol combination decides whether a projection can work at all.
//!
//! Two consequences are part of the contract:
//!
//! - Creating from a preset writes `provider_options["wireApi"]` explicitly.
//!   The legacy editor otherwise falls back to "the first protocol that has a
//!   URL", which silently re-labels an Anthropic preset as Chat Completions
//!   the next time the editor opens.
//! - A preset's model list may age. Users can add a model id by hand; the
//!   preset is never silently rewritten from the network.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ProviderModelWireApi;

/// One regional spelling of a Provider's endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPresetRegion {
    pub id: String,
    pub display_name: String,
    /// Endpoints this region replaces, keyed by wire protocol id.
    pub endpoints: BTreeMap<String, String>,
}

/// A note the preset editor shows above the model list.
///
/// Caveats are copy, not switches: they explain a limitation the preset's
/// data cannot express, and no runtime branch keys off them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPresetCaveat {
    /// The vendor's free models only answer a specific first-party client, so
    /// the preset lists paid models only.
    FreeTierNeedsClient,
    /// The plan's models are absent from the vendor's model-list endpoint and
    /// come from the plan description instead.
    ModelListExcludesPlan,
    /// Models are grouped by wire protocol rather than one flat list.
    ProtocolPerModel,
}

impl ProviderPresetCaveat {
    pub const fn id(self) -> &'static str {
        match self {
            Self::FreeTierNeedsClient => "free_tier_needs_client",
            Self::ModelListExcludesPlan => "model_list_excludes_plan",
            Self::ProtocolPerModel => "protocol_per_model",
        }
    }

    /// The message the editor shows for this caveat.
    pub const fn message(self) -> &'static str {
        match self {
            Self::FreeTierNeedsClient => {
                "Free models only work inside the vendor's own client; only paid models are listed here."
            }
            Self::ModelListExcludesPlan => {
                "Plan models are not in the vendor's model list; these come from the plan description."
            }
            Self::ProtocolPerModel => {
                "Claude models use the Anthropic API; other models use the OpenAI APIs."
            }
        }
    }
}

/// A ready-to-use Provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub id: String,
    pub display_name: String,
    /// Endpoint per wire protocol. One preset may serve several protocols
    /// through different addresses.
    pub endpoints: BTreeMap<String, String>,
    pub website: Option<String>,
    pub keys_url: Option<String>,
    /// A hint for the Key field, such as a required prefix.
    pub key_hint: Option<String>,
    /// The service runs on this machine and needs no credential.
    pub no_key: bool,
    pub regions: Vec<ProviderPresetRegion>,
    /// Model ids the preset offers out of the box.
    pub models: Vec<String>,
    /// The vendor's model-list endpoint is unusable, so a missing list falls
    /// back to `models` instead of failing.
    pub no_list: bool,
    pub endpoint_hint: Option<String>,
    /// Only model ids with this prefix belong to the plan.
    pub only: Option<String>,
    /// Static price description, when the preset is a paid plan rather than a
    /// metered API. This is vendor copy, not a computed amount.
    pub plan_note: Option<String>,
    pub caveats: Vec<ProviderPresetCaveat>,
}

impl ProviderPreset {
    /// The address for one wire protocol, with the region already applied.
    pub fn endpoint_for(&self, wire_api: ProviderModelWireApi) -> Option<&str> {
        self.endpoints
            .get(wire_api.wire_protocol_id())
            .map(String::as_str)
    }

    /// The protocol a newly created Provider should default to.
    ///
    /// The order is deliberate: Chat Completions is the widest-compatible
    /// surface, so a preset that offers it should start there, and a preset
    /// that only offers one protocol obviously starts with that one.
    pub fn default_wire_api(&self) -> Option<ProviderModelWireApi> {
        [
            ProviderModelWireApi::OpenaiChatCompletions,
            ProviderModelWireApi::AnthropicMessages,
            ProviderModelWireApi::OpenaiResponses,
            ProviderModelWireApi::GoogleGenerativeAi,
        ]
        .into_iter()
        .find(|wire_api| self.endpoint_for(*wire_api).is_some())
    }

    /// Applies a region's endpoint overrides.
    pub fn with_region(&self, region_id: Option<&str>) -> Self {
        let Some(region_id) = region_id else {
            return self.clone();
        };
        let Some(region) = self.regions.iter().find(|region| region.id == region_id) else {
            return self.clone();
        };
        let mut preset = self.clone();
        preset.endpoints.extend(
            region
                .endpoints
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        preset
    }

    /// The model list a new Provider should carry.
    ///
    /// `only` filters the vendor's list; `models` fills in when the vendor's
    /// endpoint cannot answer at all.
    pub fn offered_models(&self, fetched: &[String]) -> Vec<String> {
        let filtered = fetched
            .iter()
            .filter(|model| {
                self.only
                    .as_deref()
                    .is_none_or(|prefix| model.starts_with(prefix))
            })
            .cloned()
            .collect::<Vec<_>>();
        if filtered.is_empty() && (self.no_list || self.only.is_some()) {
            return self.models.clone();
        }
        let mut models = filtered;
        for model in &self.models {
            if !models.contains(model) {
                models.push(model.clone());
            }
        }
        models
    }
}

/// The first batch of built-in presets.
pub const PROVIDER_PRESET_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_PRESET_OPENAI: &str = "openai";
pub const PROVIDER_PRESET_GOOGLE_GEMINI: &str = "google-gemini";
pub const PROVIDER_PRESET_DEEPSEEK: &str = "deepseek";
pub const PROVIDER_PRESET_MOONSHOT_KIMI: &str = "moonshot-kimi";
pub const PROVIDER_PRESET_ZHIPU_GLM: &str = "zhipu-glm";
pub const PROVIDER_PRESET_OPENROUTER: &str = "openrouter";
pub const PROVIDER_PRESET_OLLAMA: &str = "ollama";

fn endpoints(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(protocol, url)| ((*protocol).to_string(), (*url).to_string()))
        .collect()
}

fn preset(
    id: &str,
    display_name: &str,
    endpoint_entries: &[(&str, &str)],
    models: &[&str],
) -> ProviderPreset {
    ProviderPreset {
        id: id.to_string(),
        display_name: display_name.to_string(),
        endpoints: endpoints(endpoint_entries),
        website: None,
        keys_url: None,
        key_hint: None,
        no_key: false,
        regions: Vec::new(),
        models: models.iter().map(|model| (*model).to_string()).collect(),
        no_list: false,
        endpoint_hint: None,
        only: None,
        plan_note: None,
        caveats: Vec::new(),
    }
}

/// Every preset the build ships, in display order.
pub fn provider_presets() -> Vec<ProviderPreset> {
    use ProviderModelWireApi::{
        AnthropicMessages, GoogleGenerativeAi, OpenaiChatCompletions, OpenaiResponses,
    };
    let chat = OpenaiChatCompletions.wire_protocol_id();
    let responses = OpenaiResponses.wire_protocol_id();
    let messages = AnthropicMessages.wire_protocol_id();
    let gemini = GoogleGenerativeAi.wire_protocol_id();

    let mut presets = vec![
        ProviderPreset {
            website: Some("https://www.anthropic.com".to_string()),
            keys_url: Some("https://console.anthropic.com/settings/keys".to_string()),
            key_hint: Some("starts with sk-ant-".to_string()),
            endpoint_hint: Some("Uses the Anthropic Messages API".to_string()),
            ..preset(
                PROVIDER_PRESET_ANTHROPIC,
                "Anthropic",
                &[(messages, "https://api.anthropic.com")],
                &["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"],
            )
        },
        ProviderPreset {
            website: Some("https://openai.com".to_string()),
            keys_url: Some("https://platform.openai.com/api-keys".to_string()),
            key_hint: Some("starts with sk-".to_string()),
            ..preset(
                PROVIDER_PRESET_OPENAI,
                "OpenAI",
                &[
                    (chat, "https://api.openai.com/v1"),
                    (responses, "https://api.openai.com/v1"),
                ],
                &["gpt-5", "gpt-5-mini", "o4-mini"],
            )
        },
        ProviderPreset {
            website: Some("https://ai.google.dev".to_string()),
            keys_url: Some("https://aistudio.google.com/apikey".to_string()),
            ..preset(
                PROVIDER_PRESET_GOOGLE_GEMINI,
                "Google Gemini",
                &[(gemini, "https://generativelanguage.googleapis.com")],
                &["gemini-2.5-pro", "gemini-2.5-flash"],
            )
        },
        ProviderPreset {
            website: Some("https://www.deepseek.com".to_string()),
            keys_url: Some("https://platform.deepseek.com/api_keys".to_string()),
            key_hint: Some("starts with sk-".to_string()),
            ..preset(
                PROVIDER_PRESET_DEEPSEEK,
                "DeepSeek",
                &[
                    (chat, "https://api.deepseek.com/v1"),
                    (messages, "https://api.deepseek.com/anthropic"),
                ],
                &["deepseek-chat", "deepseek-reasoner"],
            )
        },
        ProviderPreset {
            website: Some("https://platform.moonshot.cn".to_string()),
            keys_url: Some("https://platform.moonshot.cn/console/api-keys".to_string()),
            ..preset(
                PROVIDER_PRESET_MOONSHOT_KIMI,
                "Moonshot Kimi",
                &[
                    (chat, "https://api.moonshot.cn/v1"),
                    (messages, "https://api.moonshot.cn/anthropic"),
                ],
                &["kimi-k2", "kimi-k2-turbo"],
            )
        },
        ProviderPreset {
            website: Some("https://open.bigmodel.cn".to_string()),
            keys_url: Some("https://open.bigmodel.cn/usercenter/apikeys".to_string()),
            regions: vec![ProviderPresetRegion {
                id: "global".to_string(),
                display_name: "International".to_string(),
                endpoints: endpoints(&[(chat, "https://api.z.ai/api/paas/v4")]),
            }],
            ..preset(
                PROVIDER_PRESET_ZHIPU_GLM,
                "Zhipu GLM",
                &[(chat, "https://open.bigmodel.cn/api/paas/v4")],
                &["glm-4.6", "glm-4.5-air"],
            )
        },
        ProviderPreset {
            website: Some("https://openrouter.ai".to_string()),
            keys_url: Some("https://openrouter.ai/keys".to_string()),
            ..preset(
                PROVIDER_PRESET_OPENROUTER,
                "OpenRouter",
                &[
                    (chat, "https://openrouter.ai/api/v1"),
                    (responses, "https://openrouter.ai/api/v1"),
                    (messages, "https://openrouter.ai/api"),
                ],
                &[
                    "deepseek/deepseek-chat",
                    "anthropic/claude-sonnet-4.5",
                    "openai/gpt-5",
                ],
            )
        },
        ProviderPreset {
            website: Some("https://ollama.com".to_string()),
            no_key: true,
            no_list: false,
            endpoint_hint: Some("A model server on this machine; no API key is needed".to_string()),
            models: vec!["llama3.3".to_string(), "qwen3".to_string()],
            ..preset(
                PROVIDER_PRESET_OLLAMA,
                "Ollama",
                &[(chat, "http://127.0.0.1:11434/v1")],
                &[],
            )
        },
    ];
    presets.retain(|preset| {
        preset
            .default_wire_api()
            .is_some_and(|api| preset.endpoint_for(api).is_some())
    });
    presets
}

/// Looks one preset up by id.
pub fn provider_preset(id: &str) -> Option<ProviderPreset> {
    provider_presets()
        .into_iter()
        .find(|preset| preset.id == id.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_batch_is_complete_and_unique() {
        let presets = provider_presets();
        assert!(presets.len() >= 8);
        let mut ids = presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len(), "preset ids must be unique");
        for preset in &presets {
            assert!(
                preset.default_wire_api().is_some(),
                "preset {} has no usable endpoint",
                preset.id
            );
        }
    }

    #[test]
    fn deepseek_offers_both_its_apis_and_starts_on_chat() {
        let preset = provider_preset(PROVIDER_PRESET_DEEPSEEK).unwrap();
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.deepseek.com/v1")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(
            preset.default_wire_api(),
            Some(ProviderModelWireApi::OpenaiChatCompletions)
        );
    }

    #[test]
    fn local_presets_need_no_key() {
        let preset = provider_preset(PROVIDER_PRESET_OLLAMA).unwrap();
        assert!(preset.no_key);
        assert!(preset.keys_url.is_none());
    }

    #[test]
    fn region_override_rewrites_only_named_endpoints() {
        let preset = provider_preset(PROVIDER_PRESET_ZHIPU_GLM).unwrap();
        let global = preset.with_region(Some("global"));
        assert_eq!(
            global.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.z.ai/api/paas/v4")
        );
        let missing = preset.with_region(Some("nowhere"));
        assert_eq!(missing.endpoints, preset.endpoints);
    }

    #[test]
    fn offered_models_filter_and_fall_back() {
        let mut preset = preset("example", "Example", &[], &["plan-a", "plan-b"]);
        preset.only = Some("plan-".to_string());
        // A vendor list that excludes the plan still finds the plan models.
        assert_eq!(
            preset.offered_models(&["other-model".to_string()]),
            vec!["plan-a".to_string(), "plan-b".to_string()]
        );
        // A vendor list that includes a plan model keeps it, in vendor order.
        assert_eq!(
            preset.offered_models(&["plan-c".to_string(), "other".to_string()]),
            vec![
                "plan-c".to_string(),
                "plan-a".to_string(),
                "plan-b".to_string()
            ]
        );
    }

    #[test]
    fn preset_payload_has_no_money_fields_beyond_static_plan_copy() {
        for preset in provider_presets() {
            let encoded = serde_json::to_string(&preset).unwrap();
            // `planNote` is vendor copy and is allowed; computed fields are not.
            let lower = encoded.to_lowercase();
            assert!(!lower.contains("\"cost\""));
            assert!(!lower.contains("\"price\""));
            assert!(!lower.contains("\"currency\""));
        }
    }
}
