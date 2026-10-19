//! Anthropic Messages.

use serde_json::{Map, Value, json};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{
    CanonicalEvent, CanonicalMessage, CanonicalRequest, CanonicalResponse, CanonicalUsage,
    ContentPart, ImageSource, OpaqueSignature, ReasoningRequest, Role, SamplingParams, StopReason,
    ToolChoice, ToolDefinition,
};
use crate::wire::{
    StreamDecoder, StreamEncoder, ToolCallAccumulator, number_field, object, sse_frame,
    string_field, uint_field,
};

/// The `max_tokens` Anthropic requires when the client did not name one.
///
/// The protocol has no default of its own — omitting it is a 400 — so the
/// gateway has to choose. A conservative mid-range value keeps a translated
/// request valid without pretending to know a model's true ceiling.
const DEFAULT_MAX_TOKENS: u64 = 8_192;

fn blocks(content: Option<&Value>) -> GatewayResult<Vec<ContentPart>> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![ContentPart::Text(text.clone())]),
        Some(Value::Array(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                let block_type = string_field(item, "type").unwrap_or_default();
                match block_type {
                    "text" => parts.push(ContentPart::Text(
                        string_field(item, "text").unwrap_or_default().to_string(),
                    )),
                    "image" => {
                        let source = item.get("source").ok_or_else(|| {
                            GatewayError::MalformedRequest(
                                "an Anthropic image block has no source".to_string(),
                            )
                        })?;
                        let kind = string_field(source, "type").unwrap_or_default();
                        parts.push(ContentPart::Image {
                            source: match kind {
                                "base64" => ImageSource::Base64 {
                                    media_type: string_field(source, "media_type")
                                        .unwrap_or("image/png")
                                        .to_string(),
                                    data: string_field(source, "data")
                                        .unwrap_or_default()
                                        .to_string(),
                                },
                                "url" => ImageSource::Url(
                                    string_field(source, "url").unwrap_or_default().to_string(),
                                ),
                                other => {
                                    return Err(GatewayError::MalformedRequest(format!(
                                        "Anthropic image source `{other}` is not supported"
                                    )));
                                }
                            },
                        });
                    }
                    "tool_use" => parts.push(ContentPart::ToolCall {
                        id: string_field(item, "id").unwrap_or_default().to_string(),
                        name: string_field(item, "name").unwrap_or_default().to_string(),
                        arguments: item
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Map::new())),
                    }),
                    "tool_result" => {
                        let inner = blocks(item.get("content"))?;
                        parts.push(ContentPart::ToolResult {
                            call_id: string_field(item, "tool_use_id")
                                .unwrap_or_default()
                                .to_string(),
                            content: inner,
                            is_error: item
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        });
                    }
                    "thinking" => parts.push(ContentPart::Reasoning {
                        text: string_field(item, "thinking")
                            .unwrap_or_default()
                            .to_string(),
                        signature: string_field(item, "signature").map(|signature| {
                            OpaqueSignature {
                                protocol: vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                                value: signature.to_string(),
                            }
                        }),
                    }),
                    "redacted_thinking" => parts.push(ContentPart::Opaque {
                        protocol: vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                        payload: item.clone(),
                    }),
                    other => {
                        return Err(GatewayError::MalformedRequest(format!(
                            "Anthropic content block `{other}` is not supported"
                        )));
                    }
                }
            }
            Ok(parts)
        }
        Some(_) => Err(GatewayError::MalformedRequest(
            "an Anthropic message's content must be a string or an array".to_string(),
        )),
    }
}

pub fn parse_request(value: &Value) -> GatewayResult<CanonicalRequest> {
    let body = object(value, "an Anthropic request")?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if model.trim().is_empty() {
        return Err(GatewayError::MalformedRequest(
            "an Anthropic request must name a model".to_string(),
        ));
    }
    let system = blocks(body.get("system"))?;
    let mut messages = Vec::new();
    let raw = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GatewayError::MalformedRequest("an Anthropic request must carry messages".to_string())
        })?;
    for message in raw {
        let role = match string_field(message, "role") {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };
        messages.push(CanonicalMessage {
            role,
            parts: blocks(message.get("content"))?,
        });
    }

    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    Some(ToolDefinition {
                        name: string_field(tool, "name")?.to_string(),
                        description: string_field(tool, "description")
                            .unwrap_or_default()
                            .to_string(),
                        parameters: tool
                            .get("input_schema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        strict: tool.get("strict").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let tool_choice = match body.get("tool_choice") {
        Some(choice) if string_field(choice, "type") == Some("any") => ToolChoice::Required,
        Some(choice) if string_field(choice, "type") == Some("none") => ToolChoice::None,
        Some(choice) if string_field(choice, "type") == Some("tool") => {
            ToolChoice::Named(string_field(choice, "name").unwrap_or_default().to_string())
        }
        _ => ToolChoice::Auto,
    };

    let thinking = body.get("thinking");
    let reasoning = match thinking {
        Some(thinking) if string_field(thinking, "type") == Some("disabled") => ReasoningRequest {
            disabled: true,
            effort: None,
        },
        Some(thinking) if string_field(thinking, "type") == Some("enabled") => {
            // Anthropic expresses reasoning as a token budget, not a level.
            // Mapping the budget back to a level would be a guess, so the IR
            // records "reasoning on" and the encoder decides.
            let _ = uint_field(thinking, "budget_tokens");
            ReasoningRequest {
                disabled: false,
                effort: None,
            }
        }
        _ => ReasoningRequest::default(),
    };

    Ok(CanonicalRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        parallel_tool_calls: None,
        sampling: SamplingParams {
            temperature: number_field(value, "temperature"),
            top_p: number_field(value, "top_p"),
            max_tokens: uint_field(value, "max_tokens"),
            stop: value
                .get("stop_sequences")
                .and_then(Value::as_array)
                .map(|stops| {
                    stops
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        },
        reasoning,
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        opaque: Vec::new(),
    })
}

pub fn build_request(request: &CanonicalRequest, _model: &str) -> GatewayResult<Value> {
    let mut body = Map::new();
    let system = request
        .system
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) if !text.is_empty() => {
                Some(json!({"type": "text", "text": text}))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    match system.len() {
        0 => {}
        1 => {
            body.insert("system".to_string(), system[0]["text"].clone());
        }
        _ => {
            body.insert("system".to_string(), Value::Array(system));
        }
    }

    let mut messages = Vec::new();
    for message in &request.messages {
        let content = match message.role {
            Role::User => {
                let mut tool_results = Vec::new();
                for part in &message.parts {
                    if let ContentPart::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } = part
                    {
                        tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": text_or_blocks(content)?,
                            "is_error": is_error,
                        }));
                    }
                }
                let mut blocks = tool_results;
                for part in &message.parts {
                    match part {
                        ContentPart::Text(text) if !text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        ContentPart::Image { source } => {
                            if let Some(block) = image_block(source)? {
                                blocks.push(block);
                            }
                        }
                        _ => {}
                    }
                }
                blocks
            }
            Role::Assistant => message
                .parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) if !text.is_empty() => {
                        Some(Ok(json!({"type": "text", "text": text})))
                    }
                    // A thinking block without a signature cannot be sent back:
                    // Anthropic rejects the whole request. Reasoning that came
                    // from another vendor therefore degrades to text-less, and
                    // the degradation is local to the assistant turn.
                    ContentPart::Reasoning {
                        text,
                        signature: Some(signature),
                    } if signature.protocol == vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => {
                        Some(Ok(json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature.value,
                        })))
                    }
                    ContentPart::Reasoning { .. } => None,
                    ContentPart::Opaque { protocol, payload }
                        if protocol == vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES =>
                    {
                        Some(Ok(payload.clone()))
                    }
                    ContentPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(Ok(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": arguments,
                    }))),
                    ContentPart::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } => Some(text_or_blocks(content).map(|content| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": content,
                            "is_error": is_error,
                        })
                    })),
                    _ => None,
                })
                .collect::<GatewayResult<Vec<_>>>()?,
        };
        if content.is_empty() {
            continue;
        }
        messages.push(json!({
            "role": message.role.as_str(),
            "content": content,
        }));
    }

    body.insert("messages".to_string(), Value::Array(messages));
    body.insert(
        "max_tokens".to_string(),
        json!(request.sampling.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    if let Some(temperature) = request.sampling.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if !request.sampling.stop.is_empty() {
        body.insert("stop_sequences".to_string(), json!(request.sampling.stop));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.parameters,
                        })
                    })
                    .collect(),
            ),
        );
        body.insert(
            "tool_choice".to_string(),
            match &request.tool_choice {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Required => json!({"type": "any"}),
                ToolChoice::Named(name) => json!({"type": "tool", "name": name}),
            },
        );
    }
    if request.reasoning.disabled {
        body.insert("thinking".to_string(), json!({"type": "disabled"}));
    }
    body.insert("stream".to_string(), json!(request.stream));
    Ok(Value::Object(body))
}

fn text_or_blocks(parts: &[ContentPart]) -> GatewayResult<Value> {
    let text = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let has_media = parts
        .iter()
        .any(|part| matches!(part, ContentPart::Image { .. }));
    if !has_media {
        return Ok(Value::String(text));
    }
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for part in parts {
        if let ContentPart::Image { source } = part {
            if let Some(block) = image_block(source)? {
                blocks.push(block);
            }
        }
    }
    Ok(Value::Array(blocks))
}

fn image_block(source: &ImageSource) -> GatewayResult<Option<Value>> {
    Ok(Some(match source {
        ImageSource::Base64 { media_type, data } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }),
        ImageSource::Url(url) => json!({
            "type": "image",
            "source": {"type": "url", "url": url},
        }),
        ImageSource::VendorFileId(_) => return Err(GatewayError::UnsupportedImageReference),
    }))
}

fn usage_from(value: &Value) -> CanonicalUsage {
    CanonicalUsage {
        input_tokens: uint_field(value, "input_tokens").unwrap_or_default(),
        output_tokens: uint_field(value, "output_tokens").unwrap_or_default(),
        cached_read_tokens: uint_field(value, "cache_read_input_tokens").unwrap_or_default(),
        cached_write_tokens: uint_field(value, "cache_creation_input_tokens").unwrap_or_default(),
        reasoning_tokens: 0,
    }
}

pub fn render_response(response: &CanonicalResponse) -> Value {
    let mut content = Vec::new();
    for part in &response.parts {
        match part {
            ContentPart::Text(text) if !text.is_empty() => {
                content.push(json!({"type": "text", "text": text}))
            }
            ContentPart::Reasoning {
                text,
                signature: Some(signature),
            } if signature.protocol == vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => content
                .push(json!({
                    "type": "thinking",
                    "thinking": text,
                    "signature": signature.value,
                })),
            ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": arguments,
            })),
            _ => {}
        }
    }
    json!({
        "id": response.id,
        "type": "message",
        "role": "assistant",
        "model": response.model,
        "content": content,
        "stop_reason": response.stop.anthropic(),
        "stop_sequence": null,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "cache_read_input_tokens": response.usage.cached_read_tokens,
            "cache_creation_input_tokens": response.usage.cached_write_tokens,
        },
    })
}

/// Parses one whole Anthropic reply.
pub fn parse_response(value: &Value) -> GatewayResult<CanonicalResponse> {
    let content = value
        .get("content")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let parts = blocks(Some(&content))?;
    Ok(CanonicalResponse {
        id: string_field(value, "id").unwrap_or_default().to_string(),
        model: string_field(value, "model").unwrap_or_default().to_string(),
        parts,
        stop: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(StopReason::from_anthropic)
            .unwrap_or(StopReason::EndTurn),
        usage: value
            .get("usage")
            .filter(|usage| !usage.is_null())
            .map(usage_from)
            .unwrap_or_default(),
    })
}

#[derive(Default)]
pub struct AnthropicDecoder {
    tool: ToolCallAccumulator,
    thinking: bool,
    usage: CanonicalUsage,
    started: bool,
    id: String,
    model: String,
    stopped: bool,
}

impl StreamDecoder for AnthropicDecoder {
    fn decode(&mut self, data: &str) -> GatewayResult<Vec<CanonicalEvent>> {
        let value: Value = serde_json::from_str(data).map_err(|_| GatewayError::Upstream {
            status: 502,
            message: "the upstream sent an Anthropic event that is not JSON".to_string(),
        })?;
        let mut events = Vec::new();
        match string_field(&value, "type").unwrap_or_default() {
            "message_start" => {
                let message = value.get("message").unwrap_or(&Value::Null);
                self.id = string_field(message, "id").unwrap_or_default().to_string();
                self.model = string_field(message, "model")
                    .unwrap_or_default()
                    .to_string();
                if let Some(usage) = message.get("usage") {
                    self.usage.merge(&usage_from(usage));
                }
                self.started = true;
                events.push(CanonicalEvent::Start {
                    id: self.id.clone(),
                    model: self.model.clone(),
                    usage: self.usage.clone(),
                });
            }
            "content_block_start" => {
                let block = value.get("content_block").unwrap_or(&Value::Null);
                match string_field(block, "type").unwrap_or_default() {
                    "tool_use" => {
                        let id = string_field(block, "id").unwrap_or_default().to_string();
                        let name = string_field(block, "name").unwrap_or_default().to_string();
                        self.tool.start(&id, &name);
                        events.push(CanonicalEvent::ToolCallStart { id, name });
                        // Some vendors deliver the whole input on the start
                        // block. It is treated as complete; later fragments for
                        // this call are then the vendor repeating itself.
                        if let Some(input) = block.get("input") {
                            if input.as_object().is_some_and(|object| !object.is_empty()) {
                                self.tool.push_arguments(
                                    &serde_json::to_string(input).unwrap_or_default(),
                                );
                            }
                        }
                    }
                    "thinking" => {
                        self.thinking = true;
                        events.push(CanonicalEvent::ReasoningStart);
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match string_field(delta, "type").unwrap_or_default() {
                    "text_delta" => events.push(CanonicalEvent::TextDelta(
                        string_field(delta, "text").unwrap_or_default().to_string(),
                    )),
                    "thinking_delta" => {
                        if !self.thinking {
                            self.thinking = true;
                            events.push(CanonicalEvent::ReasoningStart);
                        }
                        events.push(CanonicalEvent::ReasoningDelta(
                            string_field(delta, "thinking")
                                .unwrap_or_default()
                                .to_string(),
                        ));
                    }
                    "signature_delta" => events.push(CanonicalEvent::ReasoningSignature(
                        string_field(delta, "signature")
                            .unwrap_or_default()
                            .to_string(),
                    )),
                    "input_json_delta" => {
                        let fragment = string_field(delta, "partial_json")
                            .unwrap_or_default()
                            .to_string();
                        self.tool.push_arguments(&fragment);
                        events.push(CanonicalEvent::ToolCallArgumentsDelta {
                            id: self.tool.id().to_string(),
                            fragment,
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                self.thinking = false;
                if !self.tool.is_empty() {
                    events.push(CanonicalEvent::ToolCallEnd {
                        id: self.tool.id().to_string(),
                        arguments: self.tool.arguments(),
                    });
                    self.tool = ToolCallAccumulator::default();
                }
            }
            "message_delta" => {
                if let Some(usage) = value.get("usage") {
                    self.usage.merge(&usage_from(usage));
                    events.push(CanonicalEvent::Usage(self.usage.clone()));
                }
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stopped = true;
                    events.push(CanonicalEvent::Stop(StopReason::from_anthropic(reason)));
                }
            }
            "message_stop" => {
                if !self.stopped {
                    self.stopped = true;
                    events.push(CanonicalEvent::Stop(StopReason::EndTurn));
                }
            }
            "ping" => {}
            "error" => {
                let error = value.get("error").unwrap_or(&Value::Null);
                events.push(CanonicalEvent::Error {
                    status: 502,
                    message: string_field(error, "message")
                        .unwrap_or("the upstream reported an error")
                        .to_string(),
                });
            }
            _ => {}
        }
        Ok(events)
    }

    fn finish(&mut self) -> GatewayResult<Vec<CanonicalEvent>> {
        let mut events = Vec::new();
        if !self.tool.is_empty() {
            events.push(CanonicalEvent::ToolCallEnd {
                id: self.tool.id().to_string(),
                arguments: self.tool.arguments(),
            });
            self.tool = ToolCallAccumulator::default();
        }
        Ok(events)
    }
}

pub struct AnthropicEncoder {
    model: String,
    id: String,
    block_index: u64,
    open: Option<OpenBlock>,
    usage: CanonicalUsage,
    started: bool,
    /// Set once `message_stop` has been written, so `finish` cannot repeat it.
    terminal_emitted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Text,
    Thinking,
    Tool,
}

impl AnthropicEncoder {
    pub fn new(model: String) -> Self {
        Self {
            model,
            id: String::new(),
            block_index: 0,
            open: None,
            usage: CanonicalUsage::default(),
            started: false,
            terminal_emitted: false,
        }
    }

    fn close(&mut self) -> String {
        match self.open.take() {
            Some(_) => {
                let frame = sse_frame(
                    Some("content_block_stop"),
                    &json!({"type": "content_block_stop", "index": self.block_index}),
                );
                self.block_index += 1;
                frame
            }
            None => String::new(),
        }
    }

    fn open(&mut self, block: Value, kind: OpenBlock) -> String {
        let mut output = self.close();
        output.push_str(&sse_frame(
            Some("content_block_start"),
            &json!({
                "type": "content_block_start",
                "index": self.block_index,
                "content_block": block,
            }),
        ));
        self.open = Some(kind);
        output
    }

    fn delta(&self, delta: Value) -> String {
        sse_frame(
            Some("content_block_delta"),
            &json!({
                "type": "content_block_delta",
                "index": self.block_index,
                "delta": delta,
            }),
        )
    }
}

impl StreamEncoder for AnthropicEncoder {
    fn encode(&mut self, event: &CanonicalEvent) -> GatewayResult<String> {
        Ok(match event {
            CanonicalEvent::Start { id, model, usage } => {
                self.id = id.clone();
                self.usage = usage.clone();
                if !model.is_empty() {
                    self.model = model.clone();
                }
                self.started = true;
                sse_frame(
                    Some("message_start"),
                    &json!({
                        "type": "message_start",
                        "message": {
                            "id": self.id,
                            "type": "message",
                            "role": "assistant",
                            "model": self.model,
                            "content": [],
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {
                                "input_tokens": usage.input_tokens,
                                "output_tokens": 0,
                            },
                        },
                    }),
                )
            }
            CanonicalEvent::TextDelta(text) => {
                let mut output = if self.open == Some(OpenBlock::Text) {
                    String::new()
                } else {
                    self.open(json!({"type": "text", "text": ""}), OpenBlock::Text)
                };
                output.push_str(&self.delta(json!({"type": "text_delta", "text": text})));
                output
            }
            CanonicalEvent::ReasoningStart => {
                let mut output = self.close();
                output.push_str(&sse_frame(
                    Some("content_block_start"),
                    &json!({
                        "type": "content_block_start",
                        "index": self.block_index,
                        "content_block": {"type": "thinking", "thinking": "", "signature": ""},
                    }),
                ));
                self.open = Some(OpenBlock::Thinking);
                output
            }
            CanonicalEvent::ReasoningDelta(text) => {
                let mut output = if self.open == Some(OpenBlock::Thinking) {
                    String::new()
                } else {
                    self.open(
                        json!({"type": "thinking", "thinking": "", "signature": ""}),
                        OpenBlock::Thinking,
                    )
                };
                output.push_str(&self.delta(json!({"type": "thinking_delta", "thinking": text})));
                output
            }
            CanonicalEvent::ReasoningSignature(signature) => {
                self.delta(json!({"type": "signature_delta", "signature": signature}))
            }
            CanonicalEvent::ToolCallStart { id, name } => self.open(
                json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
                OpenBlock::Tool,
            ),
            CanonicalEvent::ToolCallArgumentsDelta { fragment, .. } => {
                self.delta(json!({"type": "input_json_delta", "partial_json": fragment}))
            }
            CanonicalEvent::ToolCallEnd { .. } => self.close(),
            CanonicalEvent::Usage(usage) => {
                self.usage = usage.clone();
                // Anthropic reports output tokens on message_delta; sending a
                // standalone usage frame would confuse its SDK.
                String::new()
            }
            CanonicalEvent::Stop(reason) => {
                let mut output = self.close();
                output.push_str(&sse_frame(
                    Some("message_delta"),
                    &json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": reason.anthropic(), "stop_sequence": null},
                        "usage": {"output_tokens": self.usage.output_tokens},
                    }),
                ));
                output.push_str(&sse_frame(
                    Some("message_stop"),
                    &json!({"type": "message_stop"}),
                ));
                self.terminal_emitted = true;
                output
            }
            CanonicalEvent::Error { message, .. } => sse_frame(
                Some("error"),
                &json!({
                    "type": "error",
                    "error": {"type": "api_error", "message": message},
                }),
            ),
        })
    }

    fn finish(&mut self) -> GatewayResult<String> {
        // `Stop` already closed the open block; closing again would emit a
        // second `content_block_stop` for an index that never opened.
        let mut output = if self.terminal_emitted {
            String::new()
        } else {
            self.close()
        };
        if !self.started {
            return Ok(output);
        }
        if !self.terminal_emitted {
            output.push_str(&sse_frame(
                Some("message_stop"),
                &json!({"type": "message_stop"}),
            ));
            self.terminal_emitted = true;
        }
        Ok(output)
    }

    fn keepalive(&self) -> Option<&'static str> {
        Some("event: ping\ndata: {\"type\":\"ping\"}\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_thinking_tools_and_cache_usage() {
        let request = parse_request(&json!({
            "model": "claude-sonnet-4-5",
            "system": [{"type": "text", "text": "brief"}],
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "read"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAA"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "tool_use", "id": "call-1", "name": "read", "input": {"path": "a"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "ok"}
                ]}
            ],
            "tools": [{"name": "read", "description": "read", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "read"},
            "thinking": {"type": "enabled", "budget_tokens": 2048}
        }))
        .unwrap();
        assert_eq!(request.system.len(), 1);
        assert_eq!(request.sampling.max_tokens, Some(1024));
        assert_eq!(request.tool_choice, ToolChoice::Named("read".into()));
        assert!(matches!(
            request.messages[1].parts[0],
            ContentPart::Reasoning { .. }
        ));
        assert!(matches!(
            request.messages[2].parts[0],
            ContentPart::ToolResult { .. }
        ));
    }

    #[test]
    fn build_requires_max_tokens_and_drops_unsigned_thinking() {
        let request = CanonicalRequest {
            messages: vec![CanonicalMessage {
                role: Role::Assistant,
                parts: vec![ContentPart::Reasoning {
                    text: "from elsewhere".into(),
                    signature: None,
                }],
            }],
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn build_keeps_an_anthropic_signature() {
        let request = CanonicalRequest {
            messages: vec![CanonicalMessage {
                role: Role::Assistant,
                parts: vec![ContentPart::Reasoning {
                    text: "why".into(),
                    signature: Some(OpaqueSignature {
                        protocol: vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                        value: "sig".into(),
                    }),
                }],
            }],
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        assert_eq!(body["messages"][0]["content"][0]["signature"], "sig");
    }

    #[test]
    fn decodes_thinking_signature_and_tool_call() {
        let mut decoder = AnthropicDecoder::default();
        let mut events = Vec::new();
        for frame in [
            r#"{"type":"message_start","message":{"id":"m1","model":"claude","usage":{"input_tokens":7}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-1","name":"read"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            events.extend(decoder.decode(frame).unwrap());
        }
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::ReasoningSignature(signature) if signature == "sig"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::ToolCallEnd { arguments, .. }
                if arguments == &json!({"path": "a"})
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CanonicalEvent::Stop(StopReason::ToolUse)))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::Usage(usage) if usage.output_tokens == 3
        )));
    }

    #[test]
    fn encodes_blocks_in_order_and_stops_the_message() {
        let mut encoder = AnthropicEncoder::new("m".into());
        let mut output = String::new();
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Start {
                    id: "m1".into(),
                    model: "m".into(),
                    usage: CanonicalUsage::default(),
                })
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::TextDelta("hi".into()))
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::TextDelta("!".into()))
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Stop(StopReason::EndTurn))
                .unwrap(),
        );
        output.push_str(&encoder.finish().unwrap());
        // Count whole event lines: the JSON payload repeats the type name, so a
        // raw substring count would see every block twice.
        let events = output
            .lines()
            .filter(|line| line.starts_with("event: "))
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|line| line.contains("content_block_start"))
                .count(),
            1,
            "{output}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|line| line.contains("content_block_stop"))
                .count(),
            1,
            "{output}"
        );
        assert!(output.contains("event: message_stop"), "{output}");
    }
}
