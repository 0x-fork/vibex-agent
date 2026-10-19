//! Streaming relay.
//!
//! Two upstream shapes have to be bridged onto one client stream:
//!
//! - **Passthrough** — the upstream already speaks the client's protocol, so
//!   its bytes are forwarded unchanged and only sniffed for usage. Nothing is
//!   re-serialized, which is what keeps a vendor's prompt cache intact.
//! - **Translated** — the upstream speaks another protocol, so its SSE frames
//!   are decoded into canonical events and re-encoded for the client.
//!
//! The rule that governs both: nothing is retried after the first byte reaches
//! the client. A failure after that point becomes an in-stream error event,
//! because the Agent has already consumed a prefix of the reply.

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{CanonicalEvent, CanonicalResponse, CanonicalUsage};
use crate::wire::{StreamDecoder, WireProtocol, parse_sse};

/// The in-memory view of one reply, used for a non-streaming client and for
/// the request ledger.
#[derive(Debug, Default)]
pub struct ReplyAccumulator {
    pub id: String,
    pub model: String,
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<AccumulatedToolCall>,
    pub usage: CanonicalUsage,
    pub stop: Option<crate::ir::StopReason>,
    open_reasoning: bool,
    current_tool: Option<AccumulatedToolCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccumulatedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ReplyAccumulator {
    pub fn apply(&mut self, event: &CanonicalEvent) {
        match event {
            CanonicalEvent::Start { id, model, usage } => {
                if !id.is_empty() {
                    self.id = id.clone();
                }
                if !model.is_empty() {
                    self.model = model.clone();
                }
                self.usage.merge(usage);
            }
            CanonicalEvent::TextDelta(text) => self.text.push_str(text),
            CanonicalEvent::ReasoningStart => {
                self.open_reasoning = true;
            }
            CanonicalEvent::ReasoningDelta(text) => {
                self.open_reasoning = true;
                self.reasoning.push_str(text);
            }
            CanonicalEvent::ReasoningSignature(_) => {}
            CanonicalEvent::ToolCallStart { id, name } => {
                self.current_tool = Some(AccumulatedToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
            }
            CanonicalEvent::ToolCallArgumentsDelta { fragment, .. } => {
                if let Some(tool) = self.current_tool.as_mut() {
                    tool.arguments.push_str(fragment);
                }
            }
            CanonicalEvent::ToolCallEnd { id, arguments } => {
                let mut tool = self.current_tool.take().unwrap_or(AccumulatedToolCall {
                    id: id.clone(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if !id.is_empty() {
                    tool.id = id.clone();
                }
                if tool.arguments.trim().is_empty() {
                    tool.arguments =
                        serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into());
                }
                self.tool_calls.push(tool);
            }
            CanonicalEvent::Usage(usage) => self.usage.merge(usage),
            CanonicalEvent::Stop(reason) => self.stop = Some(*reason),
            CanonicalEvent::Error { .. } => {}
        }
    }

    pub fn finish(self) -> CanonicalResponse {
        let mut parts = Vec::new();
        if !self.reasoning.is_empty() {
            parts.push(crate::ir::ContentPart::Reasoning {
                text: self.reasoning,
                signature: None,
            });
        }
        if !self.text.is_empty() {
            parts.push(crate::ir::ContentPart::Text(self.text));
        }
        for tool in self.tool_calls {
            let arguments = serde_json::from_str(&tool.arguments)
                .unwrap_or_else(|_| Value::String(tool.arguments.clone()));
            parts.push(crate::ir::ContentPart::ToolCall {
                id: tool.id,
                name: tool.name,
                arguments,
            });
        }
        let stop = self.stop.unwrap_or(
            if parts
                .iter()
                .any(|part| matches!(part, crate::ir::ContentPart::ToolCall { .. }))
            {
                crate::ir::StopReason::ToolUse
            } else {
                crate::ir::StopReason::EndTurn
            },
        );
        CanonicalResponse {
            id: if self.id.is_empty() {
                "gateway_response".to_string()
            } else {
                self.id
            },
            model: self.model,
            parts,
            stop,
            usage: self.usage,
        }
    }

    pub fn has_reasoning(&self) -> bool {
        self.open_reasoning || !self.reasoning.is_empty()
    }
}

/// One decoded upstream byte chunk. A chunk may hold several SSE frames or a
/// partial one, so the bridge buffers until it sees a frame boundary.
#[derive(Debug, Default)]
pub struct SseBuffer {
    pending: String,
}

impl SseBuffer {
    /// Appends a chunk and returns the complete frames it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        self.pending.push_str(&String::from_utf8_lossy(chunk));
        let boundary = match self.pending.rfind("\n\n") {
            Some(boundary) => boundary + 2,
            None => return Vec::new(),
        };
        let complete = self.pending[..boundary].to_string();
        self.pending.drain(..boundary);
        parse_sse(&complete)
    }

    /// Flushes a final frame that never got its blank line.
    pub fn finish(&mut self) -> Vec<(Option<String>, String)> {
        if self.pending.trim().is_empty() {
            return Vec::new();
        }
        let remaining = std::mem::take(&mut self.pending);
        parse_sse(&remaining)
    }
}

/// What one upstream attempt produced.
#[derive(Debug)]
pub enum Attempt {
    /// Forwarded bytes, unchanged, with the usage sniffed from them.
    Passthrough {
        bytes: Vec<Bytes>,
        usage: CanonicalUsage,
    },
    /// Re-encoded frames for the client.
    Translated {
        frames: Vec<String>,
        usage: CanonicalUsage,
        response: CanonicalResponse,
    },
}

/// Sniffs usage out of a passthrough body without re-serializing it.
///
/// Every protocol reports usage on some event of its own; the sniffer only
/// looks for those, so an upstream field the gateway does not understand is
/// still forwarded verbatim.
#[derive(Debug)]
pub struct UsageSniffer {
    protocol: WireProtocol,
    usage: CanonicalUsage,
    saw_terminal: bool,
}

impl UsageSniffer {
    pub fn new(protocol: WireProtocol) -> Self {
        Self {
            protocol,
            usage: CanonicalUsage::default(),
            saw_terminal: false,
        }
    }

    pub fn observe(&mut self, frames: &[(Option<String>, String)]) {
        for (_, data) in frames {
            if data.trim() == "[DONE]" {
                self.saw_terminal = true;
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            match self.protocol {
                WireProtocol::ChatCompletions => {
                    if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
                        self.usage.merge(&CanonicalUsage {
                            input_tokens: usage
                                .get("prompt_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            output_tokens: usage
                                .get("completion_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            cached_read_tokens: usage
                                .pointer("/prompt_tokens_details/cached_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            ..CanonicalUsage::default()
                        });
                    }
                    if value
                        .pointer("/choices/0/finish_reason")
                        .and_then(Value::as_str)
                        .is_some()
                    {
                        self.saw_terminal = true;
                    }
                }
                WireProtocol::Responses => {
                    if let Some(usage) = value
                        .pointer("/response/usage")
                        .filter(|usage| !usage.is_null())
                    {
                        self.usage.merge(&CanonicalUsage {
                            input_tokens: usage
                                .get("input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            output_tokens: usage
                                .get("output_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            cached_read_tokens: usage
                                .pointer("/input_tokens_details/cached_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            ..CanonicalUsage::default()
                        });
                    }
                    if matches!(
                        value.get("type").and_then(Value::as_str),
                        Some("response.completed" | "response.incomplete" | "response.failed")
                    ) {
                        self.saw_terminal = true;
                    }
                }
                WireProtocol::AnthropicMessages => {
                    // `message_start` nests usage under `message`; every later
                    // event carries it at the top level.
                    let usage = value
                        .pointer("/message/usage")
                        .or_else(|| value.get("usage"))
                        .filter(|usage| !usage.is_null());
                    if let Some(usage) = usage {
                        self.usage.merge(&CanonicalUsage {
                            input_tokens: usage
                                .get("input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            output_tokens: usage
                                .get("output_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            cached_read_tokens: usage
                                .get("cache_read_input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            cached_write_tokens: usage
                                .get("cache_creation_input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            ..CanonicalUsage::default()
                        });
                    }
                    if matches!(
                        value.get("type").and_then(Value::as_str),
                        Some("message_stop")
                    ) {
                        self.saw_terminal = true;
                    }
                }
                WireProtocol::GeminiGenerateContent => {
                    if let Some(usage) = value.get("usageMetadata") {
                        self.usage.merge(&CanonicalUsage {
                            input_tokens: usage
                                .get("promptTokenCount")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            output_tokens: usage
                                .get("candidatesTokenCount")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            cached_read_tokens: usage
                                .get("cachedContentTokenCount")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            reasoning_tokens: usage
                                .get("thoughtsTokenCount")
                                .and_then(Value::as_u64)
                                .unwrap_or_default(),
                            ..CanonicalUsage::default()
                        });
                    }
                    if value
                        .pointer("/candidates/0/finishReason")
                        .and_then(Value::as_str)
                        .is_some()
                    {
                        self.saw_terminal = true;
                    }
                }
            }
        }
    }

    pub fn usage(&self) -> CanonicalUsage {
        self.usage.clone()
    }

    /// Whether the upstream said its reply ended. A stream that stops without
    /// one was cut, and the client must be told instead of being handed a
    /// silently truncated answer.
    pub fn saw_terminal(&self) -> bool {
        self.saw_terminal
    }

    pub fn mark_cut(&mut self) {}
}

/// Decodes an upstream SSE body into canonical events.
pub fn decode_stream(
    decoder: &mut dyn StreamDecoder,
    buffer: &mut SseBuffer,
    chunk: &[u8],
) -> GatewayResult<Vec<CanonicalEvent>> {
    let mut events = Vec::new();
    for (_, data) in buffer.push(chunk) {
        if data.trim() == "[DONE]" {
            continue;
        }
        events.extend(decoder.decode(&data)?);
    }
    Ok(events)
}

/// Reads an entire upstream body as SSE, for a non-streaming client.
pub async fn collect_stream(
    upstream: reqwest::Response,
    mut decoder: Box<dyn StreamDecoder>,
) -> GatewayResult<(Vec<CanonicalEvent>, ReplyAccumulator)> {
    let mut buffer = SseBuffer::default();
    let mut accumulator = ReplyAccumulator::default();
    let mut events = Vec::new();
    let mut stream = upstream.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| GatewayError::Upstream {
            status: 502,
            message: "the upstream stream ended before its reply was complete".to_string(),
        })?;
        for event in decode_stream(decoder.as_mut(), &mut buffer, &chunk)? {
            accumulator.apply(&event);
            events.push(event);
        }
    }
    for (_, data) in buffer.finish() {
        if data.trim() == "[DONE]" {
            continue;
        }
        for event in decoder.decode(&data)? {
            accumulator.apply(&event);
            events.push(event);
        }
    }
    for event in decoder.finish()? {
        accumulator.apply(&event);
        events.push(event);
    }
    Ok((events, accumulator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ContentPart, StopReason};
    use serde_json::json;

    #[test]
    fn sse_buffer_waits_for_a_frame_boundary() {
        let mut buffer = SseBuffer::default();
        assert!(buffer.push(b"data: {\"a\":").is_empty());
        let frames = buffer.push(b"1}\n\n");
        assert_eq!(frames, vec![(None, "{\"a\":1}".to_string())]);
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn sse_buffer_flushes_a_cut_frame_on_finish() {
        let mut buffer = SseBuffer::default();
        buffer.push(b"data: {\"a\":1}");
        let frames = buffer.finish();
        assert_eq!(frames, vec![(None, "{\"a\":1}".to_string())]);
    }

    #[test]
    fn accumulator_assembles_text_and_tools() {
        let mut accumulator = ReplyAccumulator::default();
        for event in [
            CanonicalEvent::Start {
                id: "r1".into(),
                model: "m".into(),
                usage: CanonicalUsage::default(),
            },
            CanonicalEvent::TextDelta("hel".into()),
            CanonicalEvent::TextDelta("lo".into()),
            CanonicalEvent::ToolCallStart {
                id: "call-1".into(),
                name: "read".into(),
            },
            CanonicalEvent::ToolCallArgumentsDelta {
                id: "call-1".into(),
                fragment: "{\"path\":\"a\"}".into(),
            },
            CanonicalEvent::ToolCallEnd {
                id: "call-1".into(),
                arguments: json!({"path": "a"}),
            },
            CanonicalEvent::Usage(CanonicalUsage {
                input_tokens: 4,
                output_tokens: 2,
                ..CanonicalUsage::default()
            }),
        ] {
            accumulator.apply(&event);
        }
        let response = accumulator.finish();
        assert_eq!(response.id, "r1");
        assert_eq!(response.stop, StopReason::ToolUse);
        assert_eq!(response.usage.input_tokens, 4);
        assert!(response.parts.iter().any(|part| matches!(
            part,
            ContentPart::ToolCall { name, .. } if name == "read"
        )));
    }

    #[test]
    fn sniffer_reads_usage_and_terminal_events_per_protocol() {
        let mut chat = UsageSniffer::new(WireProtocol::ChatCompletions);
        chat.observe(&[
            (
                None,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.into(),
            ),
            (
                None,
                r#"{"usage":{"prompt_tokens":9,"completion_tokens":3}}"#.into(),
            ),
        ]);
        assert_eq!(chat.usage().input_tokens, 9);
        assert!(chat.saw_terminal());

        let mut anthropic = UsageSniffer::new(WireProtocol::AnthropicMessages);
        anthropic.observe(&[
            (
                None,
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":2}}}"#.into(),
            ),
            (None, r#"{"type":"message_stop"}"#.into()),
        ]);
        assert_eq!(anthropic.usage().cached_read_tokens, 2);
        assert!(anthropic.saw_terminal());

        let mut responses = UsageSniffer::new(WireProtocol::Responses);
        responses.observe(&[(
            None,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":1}}}"#.into(),
        )]);
        assert_eq!(responses.usage().input_tokens, 7);
        assert!(responses.saw_terminal());

        let mut gemini = UsageSniffer::new(WireProtocol::GeminiGenerateContent);
        gemini.observe(&[(
            None,
            r#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2}}"#.into(),
        )]);
        assert_eq!(gemini.usage().output_tokens, 2);
        assert!(gemini.saw_terminal());
    }

    #[test]
    fn a_stream_without_its_terminal_event_is_a_cut() {
        let mut sniffer = UsageSniffer::new(WireProtocol::AnthropicMessages);
        sniffer.observe(&[(
            None,
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}"#.into(),
        )]);
        assert!(!sniffer.saw_terminal());
    }
}
