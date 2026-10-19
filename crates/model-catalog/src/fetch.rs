//! Parsing one catalogue document.
//!
//! The upstream shape is a map of provider id to its models, and each model's
//! metadata is optional and loosely typed. Every field is read defensively: a
//! value of the wrong type is dropped rather than failing the whole document,
//! because a catalogue is advisory and one malformed entry must not cost the
//! user every other suggestion.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::suggest::{Catalog, CatalogModel};

/// Why a catalogue could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogFetchError {
    #[error("the catalogue could not be reached")]
    Unreachable,
    #[error("the catalogue was not valid JSON")]
    InvalidJson,
    #[error("the catalogue had an unexpected shape")]
    InvalidShape,
}

/// The endpoint the catalogue is read from.
pub const CATALOG_URL: &str = "https://models.dev/api.json";

pub fn catalog_url() -> &'static str {
    CATALOG_URL
}

/// Parses one catalogue document into the shape the editor queries.
///
/// The document is a map of provider id to a provider object with a `models`
/// map. Anything else is an invalid shape; a single unusable model inside a
/// valid provider is skipped.
pub fn parse_catalog(body: &str) -> Result<Catalog, CatalogFetchError> {
    let value: Value = serde_json::from_str(body).map_err(|_| CatalogFetchError::InvalidJson)?;
    let providers = value.as_object().ok_or(CatalogFetchError::InvalidShape)?;
    let mut catalog = Catalog::default();
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        let provider_name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id.as_str());
        let mut entries = BTreeMap::new();
        for (model_id, model) in models {
            if let Some(entry) = read_model(model_id, model) {
                entries.insert(model_id.clone(), entry);
            }
        }
        if !entries.is_empty() {
            catalog.providers.insert(
                provider_id.clone(),
                crate::suggest::CatalogProvider {
                    id: provider_id.clone(),
                    display_name: provider_name.to_string(),
                    models: entries,
                },
            );
        }
    }
    Ok(catalog)
}

/// Reads one model entry, dropping fields that are missing or mistyped.
fn read_model(model_id: &str, model: &Value) -> Option<CatalogModel> {
    if !model.is_object() {
        return None;
    }
    let limit = model.get("limit").unwrap_or(&Value::Null);
    Some(CatalogModel {
        id: model_id.to_string(),
        display_name: model
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_tokens: limit
            .get("context")
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
        output_tokens: limit
            .get("output")
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
        reasoning: model.get("reasoning").and_then(Value::as_bool),
        image_input: model
            .get("modalities")
            .and_then(|modalities| modalities.get("input"))
            .and_then(Value::as_array)
            .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("image"))),
        pdf_input: model
            .get("modalities")
            .and_then(|modalities| modalities.get("input"))
            .and_then(Value::as_array)
            .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("pdf"))),
        temperature: model.get("temperature").and_then(Value::as_bool),
        // The catalogue does not publish the Agent's own effort vocabulary, so
        // nothing is guessed: a level table is a declaration the user makes.
        reasoning_efforts: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> &'static str {
        r#"{
            "deepseek": {
                "name": "DeepSeek",
                "models": {
                    "deepseek-chat": {
                        "name": "DeepSeek Chat",
                        "limit": {"context": 128000, "output": 8192},
                        "reasoning": false,
                        "temperature": true,
                        "modalities": {"input": ["text"]}
                    },
                    "deepseek-reasoner": {
                        "limit": {"context": 64000},
                        "reasoning": true
                    },
                    "broken": "not an object"
                }
            },
            "empty": {"name": "Empty", "models": {}},
            "shapeless": "nope"
        }"#
    }

    #[test]
    fn a_document_is_read_into_providers_and_models() {
        let catalog = parse_catalog(document()).unwrap();
        assert_eq!(catalog.providers.len(), 1, "empty providers are dropped");
        let deepseek = catalog.provider("deepseek").unwrap();
        assert_eq!(deepseek.display_name, "DeepSeek");
        assert_eq!(deepseek.models.len(), 2, "a malformed model is skipped");
        let chat = deepseek.model("deepseek-chat").unwrap();
        assert_eq!(chat.context_tokens, Some(128_000));
        assert_eq!(chat.output_tokens, Some(8_192));
        assert_eq!(chat.reasoning, Some(false));
        assert_eq!(chat.image_input, Some(false));
        assert_eq!(
            deepseek.model("deepseek-reasoner").unwrap().context_tokens,
            Some(64_000)
        );
    }

    #[test]
    fn a_mistyped_field_is_dropped_rather_than_failing_the_document() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{"limit":{"context":"huge","output":123},"reasoning":"yes"}}}}"#,
        )
        .unwrap();
        let model = catalog.provider("p").unwrap().model("m").unwrap();
        // A context that is not a number is absent, not zero.
        assert_eq!(model.context_tokens, None);
        assert_eq!(model.output_tokens, Some(123));
        assert_eq!(model.reasoning, None);
    }

    #[test]
    fn an_unusable_document_is_an_error_and_never_a_panic() {
        assert_eq!(
            parse_catalog("{").unwrap_err(),
            CatalogFetchError::InvalidJson
        );
        assert_eq!(
            parse_catalog("[]").unwrap_err(),
            CatalogFetchError::InvalidShape
        );
        assert_eq!(
            parse_catalog("null").unwrap_err(),
            CatalogFetchError::InvalidShape
        );
    }

    #[test]
    fn a_catalog_overflow_is_ignored_rather_than_truncated() {
        let catalog =
            parse_catalog(r#"{"p":{"models":{"m":{"limit":{"context":99999999999999999999}}}}}"#)
                .unwrap();
        assert_eq!(
            catalog
                .provider("p")
                .unwrap()
                .model("m")
                .unwrap()
                .context_tokens,
            None
        );
    }
}
