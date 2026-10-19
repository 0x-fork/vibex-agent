//! Wire protocols the gateway serves and the dispatch across them.
//!
//! Decoding and encoding are separate per protocol. This module owns only the
//! protocol enum, the request/reply dispatch, and the helpers every protocol
//! needs: JSON object access that fails loudly, SSE frame parsing, and the
//! increment/decrement accounting for tool-call arguments.

mod anthropic;
mod chat;
mod gemini;
mod responses;

use serde_json::{Map, Value};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalResponse};

/// One of the four APIs the gateway accepts and can emit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WireProtocol {
    #[default]
    ChatCompletions,
    Responses,
    AnthropicMessages,
    GeminiGenerateContent,
}

impl WireProtocol {
    pub const ALL: [Self; 4] = [
        Self::ChatCompletions,
        Self::Responses,
        Self::AnthropicMessages,
        Self::GeminiGenerateContent,
    ];

    pub const fn wire_protocol_id(self) -> &'static str {
        match self {
            Self::ChatCompletions => vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            Self::Responses => vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES,
            Self::AnthropicMessages => vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
            Self::GeminiGenerateContent => vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
        }
    }

    pub fn from_wire_protocol_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|protocol| protocol.wire_protocol_id() == value.trim())
    }

    /// The short name the request ledger stores. Full paths are never logged.
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat",
            Self::Responses => "responses",
            Self::AnthropicMessages => "messages",
            Self::GeminiGenerateContent => "gemini",
        }
    }

    /// The upstream path for a non-streaming call.
    pub fn path(self, model: &str, stream: bool) -> String {
        match self {
            Self::ChatCompletions => "/v1/chat/completions".to_string(),
            Self::Responses => "/v1/responses".to_string(),
            Self::AnthropicMessages => "/v1/messages".to_string(),
            Self::GeminiGenerateContent if stream => format!(
                "/v1beta/models/{}:streamGenerateContent?alt=sse",
                percent_encode_path(model)
            ),
            Self::GeminiGenerateContent => {
                format!(
                    "/v1beta/models/{}:generateContent",
                    percent_encode_path(model)
                )
            }
        }
    }

    /// The auth header this protocol's own clients send, used when the gateway
    /// needs to recognise an inbound credential it should ignore.
    pub const fn auth_header(self) -> &'static str {
        match self {
            Self::ChatCompletions | Self::Responses => "authorization",
            Self::AnthropicMessages => "x-api-key",
            Self::GeminiGenerateContent => "x-goog-api-key",
        }
    }

    pub const fn is_streaming_json(self) -> bool {
        // Every protocol streams as SSE. Gemini's `streamGenerateContent`
        // additionally requires `alt=sse` to avoid a JSON array body.
        true
    }
}

/// Everything that percent-encoding needs to leave alone in a model path.
fn percent_encode_path(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Parses an inbound body into the IR.
pub fn parse_request(protocol: WireProtocol, body: &[u8]) -> GatewayResult<CanonicalRequest> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| GatewayError::MalformedRequest("the body is not valid JSON".to_string()))?;
    match protocol {
        WireProtocol::ChatCompletions => chat::parse_request(&value),
        WireProtocol::Responses => responses::parse_request(&value),
        WireProtocol::AnthropicMessages => anthropic::parse_request(&value),
        WireProtocol::GeminiGenerateContent => gemini::parse_request(&value),
    }
}

/// Renders the IR as an upstream request body.
pub fn build_upstream_request(
    protocol: WireProtocol,
    request: &CanonicalRequest,
    model: &str,
) -> GatewayResult<Value> {
    if request.has_vendor_file_image() {
        return Err(GatewayError::UnsupportedImageReference);
    }
    let mut value = match protocol {
        WireProtocol::ChatCompletions => chat::build_request(request, model)?,
        WireProtocol::Responses => responses::build_request(request, model)?,
        WireProtocol::AnthropicMessages => anthropic::build_request(request, model)?,
        WireProtocol::GeminiGenerateContent => gemini::build_request(request, model)?,
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("model".to_string(), Value::String(model.to_string()));
        // A streamed call is the gateway's only upstream shape when the client
        // asked for a stream. A translated non-streaming call is still read as
        // a stream so its token counts are observed, then rendered whole.
        object.insert("stream".to_string(), Value::Bool(request.stream));
    }
    Ok(value)
}

/// Renders a complete reply in the protocol the client speaks.
pub fn render_response(
    protocol: WireProtocol,
    response: &CanonicalResponse,
) -> GatewayResult<Value> {
    match protocol {
        WireProtocol::ChatCompletions => Ok(chat::render_response(response)),
        WireProtocol::Responses => Ok(responses::render_response(response)),
        WireProtocol::AnthropicMessages => Ok(anthropic::render_response(response)),
        WireProtocol::GeminiGenerateContent => Ok(gemini::render_response(response)),
    }
}

/// Parses an upstream whole reply in the given protocol.
pub fn parse_reply(protocol: WireProtocol, body: &[u8]) -> GatewayResult<CanonicalResponse> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GatewayError::Upstream {
        status: 502,
        message: "the upstream reply is not valid JSON".to_string(),
    })?;
    match protocol {
        WireProtocol::ChatCompletions => chat::parse_response(&value),
        WireProtocol::Responses => responses::parse_response(&value),
        WireProtocol::AnthropicMessages => anthropic::parse_response(&value),
        WireProtocol::GeminiGenerateContent => gemini::parse_response(&value),
    }
}

/// A protocol-specific streaming decoder.
pub trait StreamDecoder: Send {
    /// Consumes one SSE `data:` payload.
    fn decode(&mut self, data: &str) -> GatewayResult<Vec<CanonicalEvent>>;

    /// Flushes anything the decoder is still holding, such as a tool call whose
    /// arguments arrived whole rather than in fragments.
    fn finish(&mut self) -> GatewayResult<Vec<CanonicalEvent>>;
}

/// A protocol-specific streaming encoder.
pub trait StreamEncoder: Send {
    /// The bytes to write for one canonical event.
    fn encode(&mut self, event: &CanonicalEvent) -> GatewayResult<String>;

    /// The terminal bytes for this protocol's stream, if it has any.
    fn finish(&mut self) -> GatewayResult<String>;

    /// The keepalive bytes this protocol tolerates. Gemini's SDK reads `data:`
    /// lines strictly, so it has none.
    fn keepalive(&self) -> Option<&'static str>;
}

pub fn decoder(protocol: WireProtocol) -> Box<dyn StreamDecoder> {
    match protocol {
        WireProtocol::ChatCompletions => Box::new(chat::ChatDecoder::default()),
        WireProtocol::Responses => Box::new(responses::ResponsesDecoder::default()),
        WireProtocol::AnthropicMessages => Box::new(anthropic::AnthropicDecoder::default()),
        WireProtocol::GeminiGenerateContent => Box::new(gemini::GeminiDecoder::default()),
    }
}

pub fn encoder(protocol: WireProtocol, request: &CanonicalRequest) -> Box<dyn StreamEncoder> {
    match protocol {
        WireProtocol::ChatCompletions => Box::new(chat::ChatEncoder::new(request.model.clone())),
        WireProtocol::Responses => {
            Box::new(responses::ResponsesEncoder::new(request.model.clone()))
        }
        WireProtocol::AnthropicMessages => {
            Box::new(anthropic::AnthropicEncoder::new(request.model.clone()))
        }
        WireProtocol::GeminiGenerateContent => {
            Box::new(gemini::GeminiEncoder::new(request.model.clone()))
        }
    }
}

/// Requires a JSON object.
fn object<'a>(value: &'a Value, what: &str) -> GatewayResult<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| GatewayError::MalformedRequest(format!("{what} must be a JSON object")))
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn uint_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

/// Parses an SSE byte stream into `(event, data)` pairs.
///
/// Vendors differ in small ways: some omit the blank line between events, some
/// send several `data:` lines that must be joined with a newline, and comments
/// carry no data at all. This parser tolerates all three and, importantly,
/// flushes a final event that arrived without its terminating blank line —
/// dropping it silently would turn a complete reply into an empty one.
pub fn parse_sse(input: &str) -> Vec<(Option<String>, String)> {
    let mut events = Vec::new();
    let mut event: Option<String> = None;
    let mut data: Vec<String> = Vec::new();

    let flush = |event: &mut Option<String>,
                 data: &mut Vec<String>,
                 out: &mut Vec<(Option<String>, String)>| {
        if data.is_empty() {
            event.take();
            return;
        }
        out.push((event.take(), data.join("\n")));
        data.clear();
    };

    for line in input.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            flush(&mut event, &mut data, &mut events);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value.to_string()),
            _ => {}
        }
    }
    flush(&mut event, &mut data, &mut events);
    events
}

/// A short, stable digest used to synthesize ids for protocols whose replies
/// omit them. It never leaves the process.
pub fn request_fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn now_seconds() -> i64 {
    vibex_core::unix_timestamp_ms() / 1000
}

/// Writes one SSE frame.
pub fn sse_frame(event: Option<&str>, data: &Value) -> String {
    let payload = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    match event {
        Some(event) => format!("event: {event}\ndata: {payload}\n\n"),
        None => format!("data: {payload}\n\n"),
    }
}

/// Accumulates a tool call whose arguments stream in fragments.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    id: String,
    name: String,
    fragments: String,
}

impl ToolCallAccumulator {
    pub fn start(&mut self, id: &str, name: &str) {
        if !id.is_empty() {
            self.id = id.to_string();
        }
        if !name.is_empty() {
            self.name = name.to_string();
        }
    }

    pub fn push_arguments(&mut self, fragment: &str) {
        self.fragments.push_str(fragment);
    }

    /// The arguments as JSON. An empty accumulation becomes `{}` because every
    /// protocol expects an object, and a tool call with no parameters is
    /// common.
    pub fn arguments(&self) -> Value {
        if self.fragments.trim().is_empty() {
            return Value::Object(Map::new());
        }
        serde_json::from_str(&self.fragments).unwrap_or_else(|_| {
            // A vendor that cut the stream leaves partial JSON. Keeping it as a
            // string is better than inventing `{}`: the Agent can report it.
            Value::String(self.fragments.clone())
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_empty(&self) -> bool {
        self.id.is_empty() && self.name.is_empty() && self.fragments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_parser_joins_data_lines_and_flushes_a_partial_event() {
        let body = "event: message_start\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: {\"c\":3}";
        let events = parse_sse(body);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].0.as_deref(), Some("message_start"));
        assert_eq!(events[0].1, "{\"a\":1}");
        assert_eq!(events[1].1, "{\"b\":2}");
        assert_eq!(events[2].1, "{\"c\":3}");
    }

    #[test]
    fn sse_parser_ignores_comments_and_handles_crlf() {
        let body = ": keepalive\r\ndata: {\"x\":1}\r\n\r\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, "{\"x\":1}");
    }

    #[test]
    fn tool_call_accumulator_parses_whole_and_partial_json() {
        let mut accumulator = ToolCallAccumulator::default();
        accumulator.start("call-1", "read");
        accumulator.push_arguments("{\"path\":");
        accumulator.push_arguments("\"a.txt\"}");
        assert_eq!(accumulator.arguments(), json!({"path": "a.txt"}));

        let mut empty = ToolCallAccumulator::default();
        empty.start("call-2", "list");
        assert_eq!(empty.arguments(), json!({}));

        let mut cut = ToolCallAccumulator::default();
        cut.start("call-3", "read");
        cut.push_arguments("{\"path\":");
        assert_eq!(cut.arguments(), Value::String("{\"path\":".to_string()));
    }

    #[test]
    fn gemini_paths_encode_the_model_and_streaming_flag() {
        assert_eq!(
            WireProtocol::GeminiGenerateContent.path("gemini-2.5-pro", false),
            "/v1beta/models/gemini-2.5-pro:generateContent"
        );
        assert_eq!(
            WireProtocol::GeminiGenerateContent.path("gemini-2.5-pro", true),
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn sse_frame_renders_named_and_unnamed_events() {
        assert_eq!(
            sse_frame(Some("ping"), &json!({"type": "ping"})),
            "event: ping\ndata: {\"type\":\"ping\"}\n\n"
        );
        assert_eq!(sse_frame(None, &json!({"x": 1})), "data: {\"x\":1}\n\n");
    }
}
