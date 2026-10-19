//! The query surface the editor uses.

use std::collections::BTreeMap;

use vibex_core::{ModelCapabilitySource, ProviderModelCapabilities, ProviderModelReasoningEfforts};

/// One model's catalogue entry.
///
/// Prices are absent by design: the product reports tokens, and a price from a
/// catalogue would be an estimate presented as a fact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub display_name: Option<String>,
    pub context_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub reasoning: Option<bool>,
    pub image_input: Option<bool>,
    pub pdf_input: Option<bool>,
    pub temperature: Option<bool>,
    pub reasoning_efforts: Option<ProviderModelReasoningEfforts>,
}

/// One provider's entries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    pub id: String,
    pub display_name: String,
    pub models: BTreeMap<String, CatalogModel>,
}

impl CatalogProvider {
    pub fn model(&self, model_id: &str) -> Option<&CatalogModel> {
        self.models.get(model_id)
    }
}

/// The whole catalogue, indexed by provider id.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub providers: BTreeMap<String, CatalogProvider>,
}

impl Catalog {
    pub fn provider(&self, provider_id: &str) -> Option<&CatalogProvider> {
        self.providers.get(provider_id)
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&CatalogModel> {
        self.provider(provider_id)?.model(model_id)
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// How a match was made, so the editor can be more cautious about a guess.
    pub fn suggestion(&self, provider_id: &str, model_id: &str) -> Option<CapabilitySuggestion> {
        if let Some(model) = self.model(provider_id, model_id) {
            return Some(CapabilitySuggestion {
                model: model.clone(),
                source: SuggestSource::Exact,
            });
        }
        // A model id is sometimes spelled with a variant suffix (`-thinking`,
        // `:free`) that the catalogue does not carry. Falling back to the
        // longest catalogue id that prefixes it recovers those without ever
        // reading a number out of the id itself.
        let provider = self.provider(provider_id)?;
        let mut best: Option<(&String, &CatalogModel)> = None;
        for (candidate, model) in &provider.models {
            let matches = model_id.starts_with(candidate.as_str())
                && model_id
                    .as_bytes()
                    .get(candidate.len())
                    .is_some_and(|byte| matches!(byte, b'-' | b':' | b'/' | b'@'));
            if matches && best.is_none_or(|(existing, _)| candidate.len() > existing.len()) {
                best = Some((candidate, model));
            }
        }
        best.map(|(_, model)| CapabilitySuggestion {
            model: model.clone(),
            source: SuggestSource::Approximate,
        })
    }
}

/// How a suggestion was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestSource {
    /// The catalogue carries this exact provider and model id.
    Exact,
    /// The catalogue carries a longer id this one extends.
    Approximate,
}

impl SuggestSource {
    pub const fn is_approximate(self) -> bool {
        matches!(self, Self::Approximate)
    }
}

/// What the catalogue suggests for one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySuggestion {
    pub model: CatalogModel,
    pub source: SuggestSource,
}

impl CapabilitySuggestion {
    /// The fields this suggestion can fill, as `(field, value)` pairs.
    ///
    /// `reasoning_efforts` is never included: the catalogue does not publish the
    /// Agent's own effort vocabulary, and guessing it would produce a
    /// declaration the Agent refuses.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();
        if let Some(context) = self.model.context_tokens {
            fields.push(("contextTokens", context.to_string()));
        }
        if let Some(output) = self.model.output_tokens {
            fields.push(("outputTokens", output.to_string()));
        }
        for (field, value) in [
            ("reasoning", self.model.reasoning),
            ("imageInput", self.model.image_input),
            ("pdfInput", self.model.pdf_input),
            ("temperature", self.model.temperature),
        ] {
            if let Some(value) = value {
                fields.push((field, value.to_string()));
            }
        }
        fields
    }

    /// The capabilities a user gets by adopting every suggested field.
    ///
    /// The result carries `Catalog` provenance for each adopted field, so a
    /// later projection can tell it apart from a hand-typed value.
    pub fn adopt_all(&self) -> ProviderModelCapabilities {
        let mut capabilities = ProviderModelCapabilities::default();
        if let Some(context) = self.model.context_tokens {
            capabilities.context_tokens = Some(context);
            capabilities.set_source("contextTokens", ModelCapabilitySource::Catalog);
        }
        if let Some(output) = self.model.output_tokens {
            capabilities.output_tokens = Some(output);
            capabilities.set_source("outputTokens", ModelCapabilitySource::Catalog);
        }
        for (field, value) in [
            ("reasoning", self.model.reasoning),
            ("imageInput", self.model.image_input),
            ("pdfInput", self.model.pdf_input),
            ("temperature", self.model.temperature),
        ] {
            if let Some(value) = value {
                match field {
                    "reasoning" => capabilities.reasoning = Some(value),
                    "imageInput" => capabilities.image_input = Some(value),
                    "pdfInput" => capabilities.pdf_input = Some(value),
                    "temperature" => capabilities.temperature = Some(value),
                    _ => {}
                }
                capabilities.set_source(field, ModelCapabilitySource::Catalog);
            }
        }
        capabilities
    }
}

/// Builds the wire shape for tests.
#[cfg(test)]
pub(crate) fn wire_suggestion_for_test(
    suggestion: &CapabilitySuggestion,
) -> vibex_core::GatewayCapabilitySuggestion {
    vibex_core::GatewayCapabilitySuggestion {
        approximate: suggestion.source.is_approximate(),
        fields: suggestion
            .fields()
            .into_iter()
            .map(|(field, value)| vibex_core::GatewayCapabilityField {
                field: field.to_string(),
                value,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::parse_catalog;

    fn catalog() -> Catalog {
        parse_catalog(
            r#"{
                "deepseek": {"name":"DeepSeek","models":{
                    "deepseek-chat":{"limit":{"context":128000,"output":8192},"reasoning":false,"temperature":true},
                    "deepseek-reasoner":{"limit":{"context":64000},"reasoning":true}
                }}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn an_exact_match_is_marked_exact() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        assert_eq!(suggestion.source, SuggestSource::Exact);
        assert_eq!(suggestion.model.context_tokens, Some(128_000));
    }

    #[test]
    fn a_suffixed_id_falls_back_to_the_longest_prefix() {
        let suggestion = catalog()
            .suggestion("deepseek", "deepseek-chat-free")
            .unwrap();
        assert_eq!(suggestion.source, SuggestSource::Approximate);
        assert!(suggestion.source.is_approximate());
        assert_eq!(suggestion.model.context_tokens, Some(128_000));
        // A prefix that is not delimited does not match.
        assert!(
            catalog()
                .suggestion("deepseek", "deepseek-chatty")
                .is_none()
        );
        // An unknown provider suggests nothing at all.
        assert!(catalog().suggestion("nowhere", "deepseek-chat").is_none());
    }

    #[test]
    fn adopting_every_field_records_catalog_provenance() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        let capabilities = suggestion.adopt_all();
        assert_eq!(capabilities.context_tokens, Some(128_000));
        assert_eq!(capabilities.output_tokens, Some(8_192));
        assert_eq!(capabilities.reasoning, Some(false));
        assert_eq!(capabilities.temperature, Some(true));
        assert_eq!(
            capabilities.adopted_from_catalog().collect::<Vec<_>>(),
            vec!["contextTokens", "outputTokens", "reasoning", "temperature"]
        );
    }

    #[test]
    fn a_hand_typed_value_is_not_overwritten_and_loses_its_catalog_mark() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        let mut capabilities = suggestion.adopt_all();
        // The user edits the context window themselves.
        capabilities.context_tokens = Some(64_000);
        capabilities.set_source("contextTokens", ModelCapabilitySource::Declared);
        assert!(
            !capabilities
                .adopted_from_catalog()
                .any(|field| field == "contextTokens")
        );
        assert_eq!(capabilities.context_tokens, Some(64_000));
        // A field with no suggestion is not adopted, and nothing else moves.
        let wire = wire_suggestion_for_test(&suggestion);
        let mut untouched = capabilities.clone();
        assert!(!wire.adopt(&mut untouched, "reasoningEfforts"));
        assert_eq!(untouched, capabilities);
    }

    #[test]
    fn reasoning_efforts_are_never_suggested() {
        let suggestion = catalog()
            .suggestion("deepseek", "deepseek-reasoner")
            .unwrap();
        assert!(suggestion.model.reasoning_efforts.is_none());
        assert!(
            !suggestion
                .fields()
                .iter()
                .any(|(field, _)| *field == "reasoningEfforts")
        );
        assert!(suggestion.adopt_all().reasoning_efforts.is_none());
    }
}
