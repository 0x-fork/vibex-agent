//! OpenAI Responses.

use serde_json::{Map, Value, json};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{
    CanonicalEvent, CanonicalMessage, CanonicalRequest, CanonicalResponse, CanonicalUsage,
    ContentPart, ImageSource, ReasoningRequest, Role, SamplingParams, StopReason, ToolChoice,
    ToolDefinition,
};
use crate::wire::{
    StreamDecoder, StreamEncoder, ToolCallAccumulator, number_field, object, sse_frame,
    string_field, uint_field,
};

fn content_parts(content: Option<&Value>) -> GatewayResult<Vec<ContentPart>> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![ContentPart::Text(text.clone())]),
        Some(Value::Array(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match string_field(item, "type").unwrap_or_default() {
                    "input_text" | "output_text" | "text" => parts.push(ContentPart::Text(
                        string_field(item, "text").unwrap_or_default().to_string(),
                    )),
                    "input_image" => {
                        let url = string_field(item, "image_url").unwrap_or_default();
                        parts.push(ContentPart::Image {
                            source: if url.starts_with("data:") {
                                match url
                                    .strip_prefix("data:")
                                    .and_then(|rest| rest.split_once(";base64,"))
                                {
                                    Some((media_type, data)) => ImageSource::Base64 {
                                        media_type: media_type.to_string(),
                                        data: data.to_string(),
                                    },
                                    None => ImageSource::Url(url.to_string()),
                                }
                            } else {
                                ImageSource::Url(url.to_string())
                            },
                        });
                    }
                    "input_file" => {
                        // A Responses file reference is vendor-side state. It is
                        // modelled so a same-protocol passthrough keeps it, and
                        // refused by any translation.
                        let file_id = string_field(item, "file_id")
                            .or_else(|| string_field(item, "file_url"))
                            .unwrap_or_default();
                        parts.push(ContentPart::Image {
                            source: ImageSource::VendorFileId(file_id.to_string()),
                        });
                    }
                    "refusal" => parts.push(ContentPart::Text(
                        string_field(item, "refusal")
                            .unwrap_or_default()
                            .to_string(),
                    )),
                    _ => {}
                }
            }
            Ok(parts)
        }
        Some(_) => Err(GatewayError::MalformedRequest(
            "a Responses content value must be a string or an array".to_string(),
        )),
    }
}

pub fn parse_request(value: &Value) -> GatewayResult<CanonicalRequest> {
    let body = object(value, "a Responses request")?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if model.trim().is_empty() {
        return Err(GatewayError::MalformedRequest(
            "a Responses request must name a model".to_string(),
        ));
    }

    let mut system = Vec::new();
    if let Some(instructions) = body.get("instructions") {
        system = content_parts(Some(instructions))?;
    }

    let mut messages = Vec::new();
    let mut pending_reasoning: Vec<ContentPart> = Vec::new();
    match body.get("input") {
        Some(Value::String(text)) => messages.push(CanonicalMessage {
            role: Role::User,
            parts: vec![ContentPart::Text(text.clone())],
        }),
        Some(Value::Array(items)) => {
            for item in items {
                let item_type = string_field(item, "type").unwrap_or("message");
                match item_type {
                    "message" => {
                        let role = match string_field(item, "role") {
                            Some("assistant") => Role::Assistant,
                            _ => Role::User,
                        };
                        let mut parts = content_parts(item.get("content"))?;
                        if role == Role::Assistant && !pending_reasoning.is_empty() {
                            let mut reasoning = std::mem::take(&mut pending_reasoning);
                            reasoning.append(&mut parts);
                            parts = reasoning;
                        }
                        messages.push(CanonicalMessage { role, parts });
                    }
                    "function_call" => {
                        let call = ContentPart::ToolCall {
                            id: string_field(item, "call_id")
                                .or_else(|| string_field(item, "id"))
                                .unwrap_or_default()
                                .to_string(),
                            name: string_field(item, "name").unwrap_or_default().to_string(),
                            arguments: item
                                .get("arguments")
                                .map(parse_arguments)
                                .unwrap_or_else(|| Value::Object(Map::new())),
                        };
                        if let Some(last) = messages.last_mut()
                            && last.role == Role::Assistant
                        {
                            last.parts.push(call);
                            continue;
                        }
                        messages.push(CanonicalMessage {
                            role: Role::Assistant,
                            parts: vec![call],
                        });
                    }
                    "function_call_output" => messages.push(CanonicalMessage {
                        role: Role::User,
                        parts: vec![ContentPart::ToolResult {
                            call_id: string_field(item, "call_id")
                                .unwrap_or_default()
                                .to_string(),
                            content: content_parts(item.get("output"))?,
                            is_error: false,
                        }],
                    }),
                    "reasoning" => {
                        let encrypted =
                            body.get("include")
                                .and_then(Value::as_array)
                                .is_some_and(|include| {
                                    include.iter().any(|entry| {
                                        entry.as_str() == Some("reasoning.encrypted_content")
                                    })
                                });
                        let summary = item
                            .pointer("/summary/0/text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if encrypted {
                            pending_reasoning.push(ContentPart::Opaque {
                                protocol: vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
                                payload: item.clone(),
                            });
                        } else if !summary.is_empty() {
                            pending_reasoning.push(ContentPart::Reasoning {
                                text: summary.to_string(),
                                signature: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    if string_field(tool, "type") != Some("function") {
                        return None;
                    }
                    Some(ToolDefinition {
                        name: string_field(tool, "name")?.to_string(),
                        description: string_field(tool, "description")
                            .unwrap_or_default()
                            .to_string(),
                        parameters: tool
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        strict: tool.get("strict").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let tool_choice = match body.get("tool_choice") {
        Some(Value::String(choice)) if choice == "none" => ToolChoice::None,
        Some(Value::String(choice)) if choice == "required" => ToolChoice::Required,
        Some(Value::Object(choice)) => string_field(&Value::Object(choice.clone()), "name")
            .map(|name| ToolChoice::Named(name.to_string()))
            .unwrap_or(ToolChoice::Auto),
        _ => ToolChoice::Auto,
    };

    let reasoning = body.get("reasoning").unwrap_or(&Value::Null);
    Ok(CanonicalRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        parallel_tool_calls: body.get("parallel_tool_calls").and_then(Value::as_bool),
        sampling: SamplingParams {
            temperature: number_field(value, "temperature"),
            top_p: number_field(value, "top_p"),
            max_tokens: uint_field(value, "max_output_tokens"),
            stop: Vec::new(),
        },
        reasoning: ReasoningRequest {
            disabled: matches!(
                reasoning.get("effort").and_then(Value::as_str),
                Some("none")
            ),
            effort: reasoning
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|effort| effort != "none"),
        },
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        opaque: body
            .get("include")
            .cloned()
            .map(|include| vec![include])
            .unwrap_or_default(),
    })
}

fn parse_arguments(value: &Value) -> Value {
    match value {
        Value::String(raw) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        other => other.clone(),
    }
}

fn input_item_parts(parts: &[ContentPart]) -> GatewayResult<Vec<Value>> {
    let mut items = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(text) => {
                items.push(json!({"type": "input_text", "text": text}));
            }
            ContentPart::Image { source } => items.push(json!({
                "type": "input_image",
                "image_url": match source {
                    ImageSource::Base64 { media_type, data } => {
                        format!("data:{media_type};base64,{data}")
                    }
                    ImageSource::Url(url) => url.clone(),
                    ImageSource::VendorFileId(id) => {
                        return Ok(vec![json!({"type": "input_file", "file_id": id})]);
                    }
                },
            })),
            ContentPart::ToolResult {
                call_id, content, ..
            } => {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": text_of(content),
                }));
            }
            _ => {}
        }
    }
    Ok(items)
}

fn text_of(parts: &[ContentPart]) -> Value {
    let text = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    Value::String(text)
}

pub fn build_request(request: &CanonicalRequest, _model: &str) -> GatewayResult<Value> {
    let mut body = Map::new();
    let system = request
        .system
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if !system.is_empty() {
        body.insert("instructions".to_string(), json!(system));
    }

    let mut items = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::User => {
                let parts = input_item_parts(&message.parts)?;
                if !parts.is_empty() {
                    items.push(json!({"type": "message", "role": "user", "content": parts}));
                }
            }
            Role::Assistant => {
                for part in &message.parts {
                    match part {
                        ContentPart::Text(text) if !text.is_empty() => {
                            items.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": text}],
                            }));
                        }
                        ContentPart::Reasoning { text, .. } if !text.is_empty() => {
                            items.push(json!({
                                "type": "reasoning",
                                "summary": [{"type": "summary_text", "text": text}],
                            }));
                        }
                        ContentPart::Opaque { protocol, payload }
                            if protocol == vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES =>
                        {
                            items.push(payload.clone());
                        }
                        ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        } => items.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": serde_json::to_string(arguments)
                                .unwrap_or_else(|_| "{}".to_string()),
                        })),
                        _ => {}
                    }
                }
            }
        }
    }
    body.insert("input".to_string(), Value::Array(items));
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                            "strict": tool.strict,
                        })
                    })
                    .collect(),
            ),
        );
        body.insert(
            "tool_choice".to_string(),
            match &request.tool_choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Named(name) => json!({"type": "function", "name": name}),
            },
        );
        if let Some(parallel) = request.parallel_tool_calls {
            body.insert("parallel_tool_calls".to_string(), json!(parallel));
        }
    }
    if let Some(temperature) = request.sampling.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(max_tokens) = request.sampling.max_tokens {
        body.insert("max_output_tokens".to_string(), json!(max_tokens));
    }
    if let Some(effort) = request.reasoning.effort.as_deref() {
        body.insert("reasoning".to_string(), json!({"effort": effort}));
    }
    body.insert("stream".to_string(), json!(request.stream));
    if !request.opaque.is_empty() {
        body.insert("include".to_string(), request.opaque[0].clone());
    }
    Ok(Value::Object(body))
}

fn usage_from(value: &Value) -> CanonicalUsage {
    CanonicalUsage {
        input_tokens: uint_field(value, "input_tokens").unwrap_or_default(),
        output_tokens: uint_field(value, "output_tokens").unwrap_or_default(),
        cached_read_tokens: value
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cached_write_tokens: 0,
        reasoning_tokens: value
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    }
}

pub fn render_response(response: &CanonicalResponse) -> Value {
    let mut output = Vec::new();
    let text = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    if !text.is_empty() {
        output.push(json!({
            "type": "message",
            "id": "msg_0",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }));
    }
    for part in &response.parts {
        if let ContentPart::ToolCall {
            id,
            name,
            arguments,
        } = part
        {
            output.push(json!({
                "type": "function_call",
                "id": id,
                "call_id": id,
                "name": name,
                "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                "status": "completed",
            }));
        }
    }
    json!({
        "id": response.id,
        "object": "response",
        "created_at": crate::wire::now_seconds(),
        "status": "completed",
        "model": response.model,
        "output": output,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "total_tokens": response.usage.total(),
            "input_tokens_details": {"cached_tokens": response.usage.cached_read_tokens},
            "output_tokens_details": {"reasoning_tokens": response.usage.reasoning_tokens},
        },
    })
}

/// Parses one whole Responses reply.
pub fn parse_response(value: &Value) -> GatewayResult<CanonicalResponse> {
    let mut parts = Vec::new();
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        for item in items {
            match string_field(item, "type").unwrap_or_default() {
                "message" => parts.extend(content_parts(item.get("content"))?),
                "reasoning" => {
                    let text = item
                        .pointer("/summary/0/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.is_empty() {
                        parts.push(ContentPart::Reasoning {
                            text: text.to_string(),
                            signature: None,
                        });
                    }
                }
                "function_call" => parts.push(ContentPart::ToolCall {
                    id: string_field(item, "call_id")
                        .or_else(|| string_field(item, "id"))
                        .unwrap_or_default()
                        .to_string(),
                    name: string_field(item, "name").unwrap_or_default().to_string(),
                    arguments: item
                        .get("arguments")
                        .map(parse_arguments)
                        .unwrap_or_else(|| Value::Object(Map::new())),
                }),
                _ => {}
            }
        }
    }
    Ok(CanonicalResponse {
        id: string_field(value, "id").unwrap_or_default().to_string(),
        model: string_field(value, "model").unwrap_or_default().to_string(),
        parts,
        stop: if string_field(value, "status") == Some("incomplete") {
            StopReason::MaxTokens
        } else {
            StopReason::EndTurn
        },
        usage: value
            .get("usage")
            .filter(|usage| !usage.is_null())
            .map(usage_from)
            .unwrap_or_default(),
    })
}

#[derive(Default)]
pub struct ResponsesDecoder {
    tool: ToolCallAccumulator,
    tool_open: bool,
    usage: CanonicalUsage,
    started: bool,
    id: String,
    model: String,
    stopped: bool,
}

impl StreamDecoder for ResponsesDecoder {
    fn decode(&mut self, data: &str) -> GatewayResult<Vec<CanonicalEvent>> {
        let value: Value = serde_json::from_str(data).map_err(|_| GatewayError::Upstream {
            status: 502,
            message: "the upstream sent a Responses event that is not JSON".to_string(),
        })?;
        let event_type = string_field(&value, "type").unwrap_or_default();
        let mut events = Vec::new();
        if event_type.starts_with("response.")
            && let Some(response) = value.get("response")
        {
            if let Some(id) = string_field(response, "id") {
                self.id = id.to_string();
            }
            if let Some(model) = string_field(response, "model") {
                self.model = model.to_string();
            }
            if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
                self.usage.merge(&usage_from(usage));
                events.push(CanonicalEvent::Usage(self.usage.clone()));
            }
        }
        if !self.started {
            self.started = true;
            events.insert(
                0,
                CanonicalEvent::Start {
                    id: self.id.clone(),
                    model: self.model.clone(),
                    usage: self.usage.clone(),
                },
            );
        }
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = string_field(&value, "delta") {
                    events.push(CanonicalEvent::TextDelta(delta.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = string_field(&value, "delta") {
                    events.push(CanonicalEvent::ReasoningDelta(delta.to_string()));
                }
            }
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                if string_field(item, "type") == Some("function_call") {
                    let id = string_field(item, "call_id")
                        .or_else(|| string_field(item, "id"))
                        .unwrap_or_default()
                        .to_string();
                    let name = string_field(item, "name").unwrap_or_default().to_string();
                    self.tool.start(&id, &name);
                    self.tool_open = true;
                    events.push(CanonicalEvent::ToolCallStart { id, name });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = string_field(&value, "delta") {
                    self.tool.push_arguments(delta);
                    events.push(CanonicalEvent::ToolCallArgumentsDelta {
                        id: self.tool.id().to_string(),
                        fragment: delta.to_string(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(arguments) = string_field(&value, "arguments") {
                    // The done event may carry a fuller argument object than the
                    // deltas did. Adopting it is safe because nothing was
                    // retracted from the client's view yet.
                    let existing = self.tool.arguments();
                    let done = parse_arguments(&Value::String(arguments.to_string()));
                    if existing != done {
                        self.tool = ToolCallAccumulator::default();
                        self.tool
                            .start(string_field(&value, "call_id").unwrap_or_default(), "");
                        self.tool.push_arguments(arguments);
                    }
                }
            }
            "response.output_item.done" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                if string_field(item, "type") == Some("function_call") && self.tool_open {
                    self.tool_open = false;
                    events.push(CanonicalEvent::ToolCallEnd {
                        id: self.tool.id().to_string(),
                        arguments: self.tool.arguments(),
                    });
                    self.tool = ToolCallAccumulator::default();
                }
            }
            "response.completed" | "response.incomplete" => {
                if !self.stopped {
                    self.stopped = true;
                    let status = value
                        .pointer("/response/status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed");
                    events.push(CanonicalEvent::Stop(if status == "incomplete" {
                        StopReason::MaxTokens
                    } else {
                        StopReason::EndTurn
                    }));
                }
            }
            "response.failed" | "error" => {
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("the upstream reported an error");
                events.push(CanonicalEvent::Error {
                    status: 502,
                    message: message.to_string(),
                });
            }
            _ => {}
        }
        Ok(events)
    }

    fn finish(&mut self) -> GatewayResult<Vec<CanonicalEvent>> {
        let mut events = Vec::new();
        if self.tool_open {
            self.tool_open = false;
            events.push(CanonicalEvent::ToolCallEnd {
                id: self.tool.id().to_string(),
                arguments: self.tool.arguments(),
            });
            self.tool = ToolCallAccumulator::default();
        }
        if !self.stopped {
            self.stopped = true;
            events.push(CanonicalEvent::Stop(StopReason::EndTurn));
        }
        Ok(events)
    }
}

pub struct ResponsesEncoder {
    model: String,
    sequence: u64,
    id: String,
    output_index: u64,
    tool: ToolCallAccumulator,
    tool_open: bool,
    usage: CanonicalUsage,
    finished: bool,
}

impl ResponsesEncoder {
    pub fn new(model: String) -> Self {
        Self {
            model,
            sequence: 0,
            id: String::new(),
            output_index: 0,
            tool: ToolCallAccumulator::default(),
            tool_open: false,
            usage: CanonicalUsage::default(),
            finished: false,
        }
    }

    fn frame(&mut self, event: &str, mut payload: Value) -> String {
        if let Some(object) = payload.as_object_mut() {
            object.insert("type".to_string(), json!(event));
            object.insert("sequence_number".to_string(), json!(self.sequence));
        }
        self.sequence += 1;
        sse_frame(Some(event), &payload)
    }
}

impl StreamEncoder for ResponsesEncoder {
    fn encode(&mut self, event: &CanonicalEvent) -> GatewayResult<String> {
        Ok(match event {
            CanonicalEvent::Start { id, model, .. } => {
                self.id = if id.is_empty() {
                    format!("resp_{}", crate::wire::request_fingerprint(id))
                } else {
                    id.clone()
                };
                if !model.is_empty() {
                    self.model = model.clone();
                }
                self.frame(
                    "response.created",
                    json!({"response": {
                        "id": self.id,
                        "object": "response",
                        "model": self.model,
                        "status": "in_progress",
                        "output": [],
                    }}),
                )
            }
            CanonicalEvent::TextDelta(text) => {
                let mut output = self.frame(
                    "response.output_item.added",
                    json!({
                        "output_index": self.output_index,
                        "item": {"id": "msg_0", "type": "message", "role": "assistant", "status": "in_progress", "content": []},
                    }),
                );
                output.push_str(&self.frame(
                    "response.output_text.delta",
                    json!({
                        "item_id": "msg_0",
                        "output_index": self.output_index,
                        "content_index": 0,
                        "delta": text,
                    }),
                ));
                output
            }
            CanonicalEvent::ReasoningStart => self.frame(
                "response.output_item.added",
                json!({
                    "output_index": self.output_index,
                    "item": {"id": "rs_0", "type": "reasoning", "summary": []},
                }),
            ),
            CanonicalEvent::ReasoningDelta(text) => {
                self.frame(
                    "response.reasoning_summary_text.delta",
                    json!({"item_id": "rs_0", "output_index": self.output_index, "summary_index": 0, "delta": text}),
                )
            }
            CanonicalEvent::ReasoningSignature(_) => String::new(),
            CanonicalEvent::ToolCallStart { id, name } => {
                self.tool.start(id, name);
                self.tool_open = true;
                self.frame(
                    "response.output_item.added",
                    json!({
                        "output_index": self.output_index,
                        "item": {
                            "id": id,
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": "",
                            "status": "in_progress",
                        },
                    }),
                )
            }
            CanonicalEvent::ToolCallArgumentsDelta { fragment, .. } => self.frame(
                "response.function_call_arguments.delta",
                json!({
                    "item_id": self.tool.id(),
                    "output_index": self.output_index,
                    "delta": fragment,
                }),
            ),
            CanonicalEvent::ToolCallEnd { id, arguments } => {
                self.tool_open = false;
                let mut output = self.frame(
                    "response.function_call_arguments.done",
                    json!({
                        "item_id": id,
                        "output_index": self.output_index,
                        "arguments": serde_json::to_string(arguments)
                            .unwrap_or_else(|_| "{}".to_string()),
                    }),
                );
                output.push_str(&self.frame(
                    "response.output_item.done",
                    json!({
                        "output_index": self.output_index,
                        "item": {
                            "id": id,
                            "type": "function_call",
                            "call_id": id,
                            "name": self.tool.name(),
                            "arguments": serde_json::to_string(arguments)
                                .unwrap_or_else(|_| "{}".to_string()),
                            "status": "completed",
                        },
                    }),
                ));
                self.output_index += 1;
                output
            }
            CanonicalEvent::Usage(usage) => {
                self.usage = usage.clone();
                String::new()
            }
            CanonicalEvent::Stop(reason) => {
                self.finished = true;
                let event = if *reason == StopReason::MaxTokens {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                self.frame(
                    event,
                    json!({"response": {
                        "id": self.id,
                        "object": "response",
                        "model": self.model,
                        "status": if *reason == StopReason::MaxTokens { "incomplete" } else { "completed" },
                        "output": [],
                        "usage": {
                            "input_tokens": self.usage.input_tokens,
                            "output_tokens": self.usage.output_tokens,
                            "total_tokens": self.usage.total(),
                            "input_tokens_details": {"cached_tokens": self.usage.cached_read_tokens},
                            "output_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
                        },
                    }}),
                )
            }
            CanonicalEvent::Error { message, .. } => self.frame(
                "response.failed",
                json!({"response": {"id": self.id, "status": "failed", "error": {"code": "upstream_error", "message": message}}}),
            ),
        })
    }

    fn finish(&mut self) -> GatewayResult<String> {
        Ok(String::new())
    }

    fn keepalive(&self) -> Option<&'static str> {
        Some("event: response.in_progress\ndata: {\"type\":\"response.in_progress\"}\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_items_and_refuses_vendor_files() {
        let request = parse_request(&json!({
            "model": "gpt-5",
            "instructions": "brief",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "read"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_file", "file_id": "file-abc"}]},
                {"type": "function_call", "call_id": "call-1", "name": "read", "arguments": "{\"path\":\"a\"}"},
                {"type": "function_call_output", "call_id": "call-1", "output": "ok"}
            ],
            "tools": [{"type": "function", "name": "read", "parameters": {"type": "object"}}],
            "max_output_tokens": 64,
            "reasoning": {"effort": "high"}
        }))
        .unwrap();
        assert_eq!(request.system.len(), 1);
        assert_eq!(request.sampling.max_tokens, Some(64));
        assert_eq!(request.reasoning.effort.as_deref(), Some("high"));
        assert!(request.has_vendor_file_image());
    }

    #[test]
    fn builds_instructions_and_function_calls() {
        let request = CanonicalRequest {
            system: vec![ContentPart::Text("brief".into())],
            messages: vec![CanonicalMessage {
                role: Role::Assistant,
                parts: vec![ContentPart::ToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                }],
            }],
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        assert_eq!(body["instructions"], "brief");
        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][0]["call_id"], "call-1");
    }

    #[test]
    fn decodes_a_function_call_stream() {
        let mut decoder = ResponsesDecoder::default();
        let mut events = Vec::new();
        for frame in [
            r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call-1","name":"read"}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"a\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call-1"}}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":9,"output_tokens":4}}}"#,
        ] {
            events.extend(decoder.decode(frame).unwrap());
        }
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::ToolCallEnd { arguments, .. }
                if arguments == &json!({"path": "a"})
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::Usage(usage) if usage.output_tokens == 4
        )));
    }

    #[test]
    fn done_event_supersedes_partial_arguments() {
        let mut decoder = ResponsesDecoder::default();
        decoder
            .decode(r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call-1","name":"read"}}"#)
            .unwrap();
        decoder
            .decode(r#"{"type":"response.function_call_arguments.delta","delta":"{\"path\":"}"#)
            .unwrap();
        let events = decoder
            .decode(r#"{"type":"response.function_call_arguments.done","call_id":"call-1","arguments":"{\"path\":\"a\",\"line\":1}"}"#)
            .unwrap();
        let _ = events;
        let finished = decoder.finish().unwrap();
        assert!(finished.iter().any(|event| matches!(
            event,
            CanonicalEvent::ToolCallEnd { arguments, .. }
                if arguments == &json!({"path": "a", "line": 1})
        )));
    }

    #[test]
    fn encoder_emits_named_events_with_sequence_numbers() {
        let mut encoder = ResponsesEncoder::new("gpt-5".into());
        let mut output = String::new();
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Start {
                    id: "resp_1".into(),
                    model: "gpt-5".into(),
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
                .encode(&CanonicalEvent::Stop(StopReason::EndTurn))
                .unwrap(),
        );
        assert!(output.contains("event: response.created"));
        assert!(output.contains("event: response.output_text.delta"));
        assert!(output.contains("event: response.completed"));
        assert!(output.contains("\"sequence_number\":0"));
    }
}
