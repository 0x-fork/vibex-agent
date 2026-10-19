//! The intermediate representation the four wire protocols convert through.
//!
//! Writing one decoder and one encoder per protocol is what keeps the matrix at
//! sixteen units instead of twelve hand-written pairs. The cost is that the IR
//! must be able to express everything any of the four can say, including the
//! parts the others cannot: reasoning that belongs to a specific vendor,
//! opaque signatures that must travel back unchanged, and cache breakpoints
//! that only one protocol has.
//!
//! Three rules make translation honest:
//!
//! 1. Irreversible information is modelled explicitly ([`ContentPart::Opaque`])
//!    instead of being re-derived at encode time.
//! 2. `system` is a field of its own, because Anthropic and Gemini put it
//!    outside the message list and Chat puts it inside.
//! 3. A part that cannot be represented by the target protocol is reported,
//!    never silently dropped.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    /// The model reached a natural end.
    #[default]
    EndTurn,
    /// The model hit `max_tokens`.
    MaxTokens,
    /// The model asked for one or more tools.
    ToolUse,
    /// A stop sequence matched.
    StopSequence,
    /// The vendor's safety filter refused or truncated the reply.
    ContentFilter,
    /// The reply ended for a reason the gateway does not model.
    Other,
}

impl StopReason {
    /// The OpenAI Chat spelling.
    pub const fn chat(self) -> &'static str {
        match self {
            Self::EndTurn => "stop",
            Self::MaxTokens => "length",
            Self::ToolUse => "tool_calls",
            Self::StopSequence => "stop",
            Self::ContentFilter => "content_filter",
            Self::Other => "stop",
        }
    }

    /// The Anthropic Messages spelling.
    pub const fn anthropic(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::ToolUse => "tool_use",
            Self::StopSequence => "stop_sequence",
            Self::ContentFilter => "refusal",
            Self::Other => "end_turn",
        }
    }

    /// The Gemini `finishReason` spelling.
    pub const fn gemini(self) -> &'static str {
        match self {
            Self::EndTurn => "STOP",
            Self::MaxTokens => "MAX_TOKENS",
            Self::ToolUse => "STOP",
            Self::StopSequence => "STOP",
            Self::ContentFilter => "SAFETY",
            Self::Other => "OTHER",
        }
    }

    pub fn from_anthropic(value: &str) -> Self {
        match value {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "tool_use" => Self::ToolUse,
            "stop_sequence" => Self::StopSequence,
            "refusal" => Self::ContentFilter,
            _ => Self::Other,
        }
    }

    pub fn from_gemini(value: &str) -> Self {
        match value {
            "STOP" => Self::EndTurn,
            "MAX_TOKENS" => Self::MaxTokens,
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => Self::ContentFilter,
            _ => Self::Other,
        }
    }

    pub fn from_chat(value: &str) -> Self {
        match value {
            "stop" => Self::EndTurn,
            "length" => Self::MaxTokens,
            "tool_calls" | "function_call" => Self::ToolUse,
            "content_filter" => Self::ContentFilter,
            _ => Self::Other,
        }
    }
}

/// One block of a message.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text(String),
    /// Reasoning the model produced. `vendor` is the protocol that owns the
    /// accompanying signature, when there is one.
    Reasoning {
        text: String,
        signature: Option<OpaqueSignature>,
    },
    Image {
        source: ImageSource,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        content: Vec<ContentPart>,
        is_error: bool,
    },
    /// A field the source protocol produced that must travel back to the same
    /// protocol unchanged. `protocol` names the owning wire protocol.
    Opaque {
        protocol: String,
        payload: Value,
    },
}

/// Where an image's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// Inline base64 with its media type.
    Base64 { media_type: String, data: String },
    /// A publicly reachable URL. Some upstreams fetch it themselves.
    Url(String),
    /// A vendor-side file identifier.
    ///
    /// This cannot cross Providers: `file-abc` exists only at the vendor that
    /// issued it. It is kept so that a same-protocol passthrough can carry it
    /// unchanged, and a translation refuses it.
    VendorFileId(String),
}

/// An opaque signature, kept with the protocol that issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueSignature {
    pub protocol: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalMessage {
    pub role: Role,
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// The client asked for strict schema adherence.
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

/// Sampling parameters shared by every protocol. `None` means "the client did
/// not ask", which is distinct from "the client asked for the default".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SamplingParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u64>,
    pub stop: Vec<String>,
}

/// Reasoning request as the client expressed it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReasoningRequest {
    /// The client turned visible reasoning off explicitly.
    pub disabled: bool,
    /// A level the client asked for, in the Agent's own vocabulary.
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalRequest {
    /// The model id the client asked for, which may carry a Provider prefix.
    pub model: String,
    pub system: Vec<ContentPart>,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: Option<bool>,
    pub sampling: SamplingParams,
    pub reasoning: ReasoningRequest,
    pub stream: bool,
    /// Fields the source protocol must get back verbatim.
    pub opaque: Vec<Value>,
}

impl CanonicalRequest {
    /// A request with nothing but a model, for the `/v1/models` path and tests.
    pub fn for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            sampling: SamplingParams::default(),
            reasoning: ReasoningRequest::default(),
            stream: false,
            opaque: Vec::new(),
        }
    }

    /// True when any part of the request is an image the target protocol
    /// cannot address.
    pub fn has_vendor_file_image(&self) -> bool {
        fn any_in(parts: &[ContentPart]) -> bool {
            parts.iter().any(|part| match part {
                ContentPart::Image {
                    source: ImageSource::VendorFileId(_),
                } => true,
                ContentPart::ToolResult { content, .. } => any_in(content),
                _ => false,
            })
        }
        any_in(&self.system) || self.messages.iter().any(|message| any_in(&message.parts))
    }

    /// The text of the first user turn, used to derive a conversation key.
    pub fn first_user_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .filter(|message| message.role == Role::User)
            .find_map(|message| {
                message.parts.iter().find_map(|part| match part {
                    ContentPart::Text(text) if !text.trim().is_empty() => Some(text.as_str()),
                    _ => None,
                })
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_read_tokens: u64,
    pub cached_write_tokens: u64,
    pub reasoning_tokens: u64,
}

impl CanonicalUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_read_tokens)
            .saturating_add(self.cached_write_tokens)
            .saturating_add(self.reasoning_tokens)
    }

    /// Folds an incremental update in. Vendors repeat cumulative counters on
    /// later events, so each field takes the larger value rather than summing.
    pub fn merge(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cached_read_tokens = self.cached_read_tokens.max(other.cached_read_tokens);
        self.cached_write_tokens = self.cached_write_tokens.max(other.cached_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.max(other.reasoning_tokens);
    }
}

/// A complete reply, as the collector assembles it from stream events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub parts: Vec<ContentPart>,
    pub stop: StopReason,
    pub usage: CanonicalUsage,
}

/// One thing a streaming reply said.
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalEvent {
    Start {
        id: String,
        model: String,
        usage: CanonicalUsage,
    },
    TextDelta(String),
    ReasoningStart,
    ReasoningDelta(String),
    /// A signature belonging to the reasoning block just streamed.
    ReasoningSignature(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgumentsDelta {
        id: String,
        fragment: String,
    },
    ToolCallEnd {
        id: String,
        arguments: Value,
    },
    Usage(CanonicalUsage),
    Stop(StopReason),
    Error {
        status: u16,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_merge_takes_cumulative_maximums() {
        let mut usage = CanonicalUsage {
            input_tokens: 10,
            output_tokens: 2,
            ..CanonicalUsage::default()
        };
        usage.merge(&CanonicalUsage {
            input_tokens: 10,
            output_tokens: 7,
            cached_read_tokens: 3,
            ..CanonicalUsage::default()
        });
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cached_read_tokens, 3);
        assert_eq!(usage.total(), 20);
    }

    #[test]
    fn vendor_file_images_are_detected_in_tool_results() {
        let request = CanonicalRequest {
            messages: vec![CanonicalMessage {
                role: Role::User,
                parts: vec![ContentPart::ToolResult {
                    call_id: "call-1".into(),
                    is_error: false,
                    content: vec![ContentPart::Image {
                        source: ImageSource::VendorFileId("file-abc".into()),
                    }],
                }],
            }],
            ..CanonicalRequest::for_model("m")
        };
        assert!(request.has_vendor_file_image());
    }

    #[test]
    fn stop_reasons_round_trip_through_each_spelling() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::ToolUse,
            StopReason::StopSequence,
            StopReason::ContentFilter,
        ] {
            assert_eq!(StopReason::from_anthropic(reason.anthropic()), reason);
        }
        assert_eq!(
            StopReason::from_chat(StopReason::MaxTokens.chat()),
            StopReason::MaxTokens
        );
        assert_eq!(
            StopReason::from_gemini(StopReason::MaxTokens.gemini()),
            StopReason::MaxTokens
        );
    }
}
