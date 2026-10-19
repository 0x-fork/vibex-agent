//! A maintained catalogue of model metadata.
//!
//! The Provider spec forbids *inferring* a capability from a model id, an
//! endpoint, or a vendor. It does not forbid *data*. A catalogue is data: a
//! maintained table that says "this exact model has this context window", the
//! same kind of statement a user makes when they type it by hand.
//!
//! Three rules follow from that distinction and shape this crate:
//!
//! 1. A catalogue value is a **suggestion**, never an applied default. The user
//!    adopts it per field, and the adoption is recorded as provenance.
//! 2. A hand-typed value always wins. Nothing here overwrites a declaration.
//! 3. Every failure is silent. A catalogue that cannot be fetched, parsed, or
//!    read changes no capability and blocks nothing; the editor simply shows no
//!    suggestions.
//!
//! Prices are deliberately not modelled. The product reports tokens, not money.

mod cache;
mod fetch;
mod suggest;

pub use cache::{CatalogCache, CatalogState};
pub use fetch::{CatalogFetchError, catalog_url, parse_catalog};
pub use suggest::{CapabilitySuggestion, Catalog, CatalogModel, SuggestSource};

/// How long a fetched catalogue is trusted before it is refreshed.
pub const CATALOG_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// The catalogue's own schema version, so a stored cache can be invalidated.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    /// No catalogue contract may carry a money field.
    ///
    /// The product reports tokens; a price is an estimate that misleads more
    /// than it informs, and an existing contract test already forbids it in
    /// usage. This keeps the catalogue from reintroducing it by the side door.
    #[test]
    fn a_catalog_model_carries_no_money_field() {
        let model = CatalogModel {
            id: "deepseek-chat".to_string(),
            display_name: Some("DeepSeek Chat".to_string()),
            context_tokens: Some(128_000),
            output_tokens: Some(8_192),
            reasoning: Some(true),
            image_input: Some(false),
            pdf_input: Some(false),
            temperature: Some(true),
            reasoning_efforts: None,
        };
        let encoded = serde_json::to_string(&model).unwrap().to_lowercase();
        for forbidden in ["cost", "price", "currency", "amount", "usd", "micros"] {
            assert!(
                !encoded.contains(forbidden),
                "a catalog model must not carry `{forbidden}`: {encoded}"
            );
        }
    }
}
