//! The gateway's error surface.
//!
//! Each variant carries the HTTP status and a stable code, so the response the
//! Agent receives stays explainable without leaking upstream bodies. A closed
//! variant set is what keeps that mapping honest.

use std::fmt;

use vibex_core::{GatewayFailureKind, VibexError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// No upstream serves the requested model.
    ModelNotFound {
        model: String,
        closest: Vec<String>,
    },
    /// The requested model is served, but not by any Key that may answer now.
    AllKeysCooling {
        model: String,
        retry_after_ms: Option<i64>,
    },
    /// Every upstream serving the model has crossed its local token ceiling.
    TokenCeilingReached {
        provider: String,
    },
    /// The model's Provider does not speak a protocol this request can become.
    TranslationDisabled {
        wire_protocol_id: String,
    },
    UnsupportedImageReference,
    /// The inbound body is not a request this protocol understands.
    MalformedRequest(String),
    /// Something upstream failed in a way the caller should see.
    Upstream {
        status: u16,
        message: String,
    },
    /// The endpoint answered as if this request belonged to another protocol.
    ///
    /// It is its own variant because it is not a Key or provider failure: the
    /// credential worked, the provider is reachable, and the same request may
    /// still succeed on a different protocol. Counting it against the Key or
    /// the route group would report the wrong thing and, with one Key, would
    /// make the mistake unrecoverable.
    ProtocolMismatch {
        wire_protocol_id: String,
        status: u16,
        message: String,
    },
    /// The gateway rebuilt a same-protocol body and the vendor refused its
    /// shape.
    ///
    /// It is distinct from [`GatewayError::ProtocolMismatch`] because the
    /// protocol was *not* in question: the client spoke what the endpoint
    /// speaks, and the refusal names a field the gateway's own encoder got
    /// wrong. Blaming the protocol sends the reader to a setting that cannot
    /// fix anything, and — for an endpoint whose protocol was only a guess —
    /// records the wrong answer for every later request.
    TranslationFailed {
        wire_protocol_id: String,
        status: u16,
        message: String,
    },
    /// The route's rules paused routing for this request.
    ///
    /// A pause is the user's own decision, not an upstream failure: the turn
    /// stops before any Provider is called and the message says which rule did
    /// it. It must not cool a Key or count toward a group's breaker.
    RoutePaused {
        group: String,
        reason: String,
    },
    /// The gateway's own configuration is unusable.
    NotConfigured(String),
    /// An internal invariant broke. Never includes request content.
    Internal(String),
}

impl GatewayError {
    pub const fn status(&self) -> u16 {
        match self {
            Self::ModelNotFound { .. } => 404,
            Self::AllKeysCooling { .. } => 429,
            Self::TokenCeilingReached { .. } => 429,
            Self::TranslationDisabled { .. } => 400,
            Self::UnsupportedImageReference => 400,
            Self::MalformedRequest(_) => 400,
            Self::Upstream { status, .. } => *status,
            Self::ProtocolMismatch { .. } | Self::TranslationFailed { .. } => 502,
            Self::RoutePaused { .. } => 503,
            Self::NotConfigured(_) => 503,
            Self::Internal(_) => 500,
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::ModelNotFound { .. } => "model_not_found",
            Self::AllKeysCooling { .. } => "model_gateway_keys_cooling",
            Self::TokenCeilingReached { .. } => "model_gateway_token_ceiling_reached",
            Self::TranslationDisabled { .. } => "model_gateway_translation_disabled",
            Self::UnsupportedImageReference => "unsupported_image_reference",
            Self::MalformedRequest(_) => "model_gateway_request_invalid",
            Self::Upstream { .. } => "model_gateway_upstream_failed",
            Self::ProtocolMismatch { .. } => "model_gateway_protocol_mismatch",
            Self::TranslationFailed { .. } => "model_gateway_translation_failed",
            Self::RoutePaused { .. } => "model_gateway_route_paused",
            Self::NotConfigured(_) => "model_gateway_not_configured",
            Self::Internal(_) => "model_gateway_internal",
        }
    }

    /// The message a user sees. Upstream messages are passed through because
    /// the vendor's own words are the most useful diagnostic, but they are
    /// truncated so a huge HTML error page cannot reach the Agent.
    pub fn message(&self) -> String {
        match self {
            Self::ModelNotFound { model, closest } if closest.is_empty() => {
                format!("No configured Provider serves the model `{model}`.")
            }
            Self::ModelNotFound { model, closest } => format!(
                "No configured Provider serves the model `{model}`. Closest configured models: {}.",
                closest.join(", ")
            ),
            Self::AllKeysCooling { model, .. } => format!(
                "Every Key serving `{model}` is cooling down after a failure. Requests will resume when one recovers."
            ),
            Self::TokenCeilingReached { provider } => format!(
                "`{provider}` reached the token ceiling you set. Requests will resume when the window rolls over."
            ),
            Self::TranslationDisabled { wire_protocol_id } => format!(
                "This request needs a translation to `{wire_protocol_id}`, which is turned off for this Provider."
            ),
            Self::UnsupportedImageReference => {
                "This request references a vendor-side file, which the gateway cannot forward across Providers. Upload the image directly, or use a direct Provider for this model.".to_string()
            }
            Self::MalformedRequest(detail) => format!("The gateway could not read this request: {detail}"),
            Self::Upstream { message, .. } => message.clone(),
            Self::ProtocolMismatch {
                wire_protocol_id,
                message,
                ..
            } => format!(
                "`{wire_protocol_id}` is the wrong protocol for this endpoint: {message}"
            ),
            Self::TranslationFailed {
                wire_protocol_id,
                message,
                ..
            } => format!(
                "The gateway could not build a `{wire_protocol_id}` request this endpoint accepts: {message}"
            ),
            Self::RoutePaused { group, reason } => format!(
                "Routing for `{group}` is paused: {reason}"
            ),
            Self::NotConfigured(detail) => format!("The gateway is not configured: {detail}"),
            Self::Internal(detail) => format!("The gateway could not complete this request: {detail}"),
        }
    }

    /// What the failure means for the Key that produced it, when it came from
    /// upstream. `None` means the Key is fine and the request itself was wrong.
    pub fn key_failure(&self) -> Option<GatewayFailureKind> {
        match self {
            Self::Upstream { status, .. } => Some(classify_status(*status)),
            _ => None,
        }
    }

    /// Whether this failure says anything about the upstream or the Key.
    ///
    /// A protocol mismatch, a translation the gateway got wrong, and the
    /// gateway's own refusals all say nothing about either: counting them is
    /// what let one wrong-protocol guess cool a Key and then degrade the whole
    /// gateway.
    pub const fn blames_upstream(&self) -> bool {
        matches!(
            self,
            Self::Upstream { .. } | Self::AllKeysCooling { .. } | Self::TokenCeilingReached { .. }
        )
    }

    pub fn retry_after_ms(&self) -> Option<i64> {
        match self {
            Self::AllKeysCooling { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// Whether retrying the same request later could succeed.
    ///
    /// Only transport-shaped and capacity failures qualify: a bad credential,
    /// a missing model, a wrong protocol, and a body the gateway could not
    /// build are all deterministic, so patience would only delay the error.
    pub const fn is_recoverable(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => {
                matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529)
            }
            Self::AllKeysCooling { .. } => true,
            _ => false,
        }
    }
}

fn classify_status(status: u16) -> GatewayFailureKind {
    match status {
        401 | 403 => GatewayFailureKind::Auth,
        402 => GatewayFailureKind::Credit,
        408 | 504 => GatewayFailureKind::Upstream,
        429 => GatewayFailureKind::Rate,
        502 | 503 | 529 => GatewayFailureKind::Proxy,
        _ => GatewayFailureKind::Other,
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for GatewayError {}

impl From<GatewayError> for VibexError {
    fn from(error: GatewayError) -> Self {
        let mut mapped = VibexError::provider(error.code(), error.message());
        mapped = mapped.with_diagnostic("httpStatus", error.status().to_string());
        mapped
    }
}

impl From<VibexError> for GatewayError {
    fn from(error: VibexError) -> Self {
        Self::Internal(error.code)
    }
}

pub type GatewayResult<T> = Result<T, GatewayError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_code_are_stable_per_variant() {
        assert_eq!(
            GatewayError::ModelNotFound {
                model: "x".into(),
                closest: Vec::new()
            }
            .status(),
            404
        );
        assert_eq!(
            GatewayError::UnsupportedImageReference.code(),
            "unsupported_image_reference"
        );
        assert_eq!(GatewayError::UnsupportedImageReference.status(), 400);
        assert_eq!(
            GatewayError::Upstream {
                status: 401,
                message: "bad key".into()
            }
            .key_failure(),
            Some(GatewayFailureKind::Auth)
        );
        assert_eq!(
            GatewayError::MalformedRequest("x".into()).key_failure(),
            None
        );
        assert!(
            !GatewayError::ProtocolMismatch {
                wire_protocol_id: "openai_responses".into(),
                status: 422,
                message: "unknown variant".into(),
            }
            .blames_upstream(),
            "a wrong protocol must not be charged to the upstream"
        );
    }

    /// A body the gateway built wrongly is reported as its own failure.
    ///
    /// It must read differently from a wrong protocol, because the protocol was
    /// not in question and no Provider setting can fix it. It also must not be
    /// charged to the Key or to the route group, or one encoder bug would cool
    /// every Key and step the group aside.
    #[test]
    fn a_translation_failure_is_the_gateways_own_problem() {
        let error = GatewayError::TranslationFailed {
            wire_protocol_id: "openai_responses".into(),
            status: 422,
            message: "unknown variant `function_call_output`".into(),
        };
        assert_eq!(error.code(), "model_gateway_translation_failed");
        assert_eq!(error.status(), 502);
        assert_eq!(error.key_failure(), None);
        assert!(!error.blames_upstream());
        let message = error.message();
        assert!(message.contains("could not build"), "got {message}");
        assert!(
            !message.contains("wrong protocol"),
            "a broken body must not be described as a wrong protocol: {message}"
        );
    }

    #[test]
    fn file_id_message_explains_the_next_step() {
        let message = GatewayError::UnsupportedImageReference.message();
        assert!(message.contains("cannot forward"));
        assert!(message.contains("Upload the image directly"));
    }
}
