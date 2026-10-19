//! OpenAI Chat Completions.

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

fn parts_from_content(content: Option<&Value>) -> GatewayResult<Vec<ContentPart>> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![ContentPart::Text(text.clone())]),
        Some(Value::Array(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                let part_type = string_field(item, "type").unwrap_or_default();
                match part_type {
                    "text" | "input_text" | "output_text" => parts.push(ContentPart::Text(
                        string_field(item, "text").unwrap_or_default().to_string(),
                    )),
                    "image_url" => {
                        let url = item
                            .get("image_url")
                            .and_then(|image| {
                                image
                                    .as_str()
                                    .map(str::to_string)
                                    .or_else(|| string_field(image, "url").map(str::to_string))
                            })
                            .ok_or_else(|| {
                                GatewayError::MalformedRequest(
                                    "a Chat image part has no URL".to_string(),
                                )
                            })?;
                        parts.push(ContentPart::Image {
                            source: image_source(&url),
                        });
                    }
                    // A refusal is text the model produced.
                    "refusal" => parts.push(ContentPart::Text(
                        string_field(item, "refusal")
                            .unwrap_or_default()
                            .to_string(),
                    )),
                    other => {
                        return Err(GatewayError::MalformedRequest(format!(
                            "Chat content part `{other}` is not supported"
                        )));
                    }
                }
            }
            Ok(parts)
        }
        Some(_) => Err(GatewayError::MalformedRequest(
            "a Chat message's content must be a string or an array".to_string(),
        )),
    }
}

fn image_source(url: &str) -> ImageSource {
    match url.strip_prefix("data:") {
        Some(rest) => match rest.split_once(";base64,") {
            Some((media_type, data)) => ImageSource::Base64 {
                media_type: media_type.to_string(),
                data: data.to_string(),
            },
            None => ImageSource::Url(url.to_string()),
        },
        None => ImageSource::Url(url.to_string()),
    }
}

pub fn parse_request(value: &Value) -> GatewayResult<CanonicalRequest> {
    let body = object(value, "a Chat request")?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if model.trim().is_empty() {
        return Err(GatewayError::MalformedRequest(
            "a Chat request must name a model".to_string(),
        ));
    }

    let mut system = Vec::new();
    let mut messages = Vec::new();
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GatewayError::MalformedRequest("a Chat request must carry messages".to_string())
        })?;
    for message in raw_messages {
        let role = string_field(message, "role").unwrap_or("user");
        match role {
            "system" | "developer" => {
                system.extend(parts_from_content(message.get("content"))?);
            }
            "user" => messages.push(CanonicalMessage {
                role: Role::User,
                parts: parts_from_content(message.get("content"))?,
            }),
            "tool" | "function" => {
                let call_id = string_field(message, "tool_call_id")
                    .unwrap_or_default()
                    .to_string();
                messages.push(CanonicalMessage {
                    role: Role::User,
                    parts: vec![ContentPart::ToolResult {
                        call_id,
                        content: parts_from_content(message.get("content"))?,
                        is_error: false,
                    }],
                });
            }
            "assistant" => {
                let mut parts = Vec::new();
                if let Some(reasoning) = string_field(message, "reasoning_content") {
                    if !reasoning.is_empty() {
                        parts.push(ContentPart::Reasoning {
                            text: reasoning.to_string(),
                            signature: None,
                        });
                    }
                }
                parts.extend(parts_from_content(message.get("content"))?);
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let function = call.get("function").unwrap_or(&Value::Null);
                        parts.push(ContentPart::ToolCall {
                            id: string_field(call, "id").unwrap_or_default().to_string(),
                            name: string_field(function, "name")
                                .unwrap_or_default()
                                .to_string(),
                            arguments: arguments_value(function.get("arguments")),
                        });
                    }
                }
                messages.push(CanonicalMessage {
                    role: Role::Assistant,
                    parts,
                });
            }
            other => {
                return Err(GatewayError::MalformedRequest(format!(
                    "Chat role `{other}` is not supported"
                )));
            }
        }
    }

    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    let function = tool.get("function")?;
                    Some(ToolDefinition {
                        name: string_field(function, "name")?.to_string(),
                        description: string_field(function, "description")
                            .unwrap_or_default()
                            .to_string(),
                        parameters: function
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        strict: function
                            .get("strict")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let tool_choice = match body.get("tool_choice") {
        Some(Value::String(choice)) => match choice.as_str() {
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            _ => ToolChoice::Auto,
        },
        Some(Value::Object(choice)) => choice
            .get("function")
            .and_then(|function| string_field(function, "name"))
            .map(|name| ToolChoice::Named(name.to_string()))
            .unwrap_or(ToolChoice::Auto),
        _ => ToolChoice::Auto,
    };

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
            max_tokens: uint_field(value, "max_completion_tokens")
                .or(uint_field(value, "max_tokens")),
            stop: stop_sequences(body.get("stop")),
        },
        reasoning: ReasoningRequest {
            disabled: matches!(
                body.get("reasoning_effort").and_then(Value::as_str),
                Some("none")
            ),
            effort: body
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|effort| effort != "none"),
        },
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        opaque: Vec::new(),
    })
}

fn stop_sequences(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(stop)) => vec![stop.clone()],
        Some(Value::Array(stops)) => stops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn arguments_value(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        Some(other) => other.clone(),
        None => Value::Object(Map::new()),
    }
}

fn content_value(parts: &[ContentPart]) -> GatewayResult<Option<Value>> {
    let text_only = parts
        .iter()
        .all(|part| matches!(part, ContentPart::Text(_)));
    if text_only {
        let text = parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        return Ok(if text.is_empty() {
            None
        } else {
            Some(Value::String(text))
        });
    }
    let mut items = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            ContentPart::Text(text) => items.push(json!({"type": "text", "text": text})),
            ContentPart::Image { source } => {
                let url = image_url(source)?;
                items.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            ContentPart::Reasoning { .. } | ContentPart::Opaque { .. } => {}
            ContentPart::ToolCall { .. } | ContentPart::ToolResult { .. } => {
                return Err(GatewayError::MalformedRequest(
                    "Chat message content cannot hold a tool block".to_string(),
                ));
            }
        }
    }
    Ok(Some(Value::Array(items)))
}

fn image_url(source: &ImageSource) -> GatewayResult<String> {
    match source {
        ImageSource::Base64 { media_type, data } => Ok(format!("data:{media_type};base64,{data}")),
        ImageSource::Url(url) => Ok(url.clone()),
        ImageSource::VendorFileId(_) => Err(GatewayError::UnsupportedImageReference),
    }
}

pub fn build_request(request: &CanonicalRequest, _model: &str) -> GatewayResult<Value> {
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": content_value(&request.system)?,
        }));
    }
    for message in &request.messages {
        match message.role {
            Role::User => {
                // Tool results are their own message in Chat, and must come
                // before any user text that follows them in the same turn.
                for part in &message.parts {
                    if let ContentPart::ToolResult {
                        call_id, content, ..
                    } = part
                    {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": content_value(content)?
                                .unwrap_or(Value::String(String::new())),
                        }));
                    }
                }
                let rest = message
                    .parts
                    .iter()
                    .filter(|part| !matches!(part, ContentPart::ToolResult { .. }))
                    .cloned()
                    .collect::<Vec<_>>();
                if !rest.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": content_value(&rest)?,
                    }));
                }
            }
            Role::Assistant => {
                let reasoning = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Reasoning { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let text = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let calls = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(arguments)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            },
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                // An assistant turn with neither text nor calls is rejected by
                // several vendors; it only exists here because the source
                // protocol modelled an empty reply.
                if text.is_empty() && calls.is_empty() && reasoning.is_empty() {
                    continue;
                }
                let mut message = Map::new();
                message.insert("role".to_string(), json!("assistant"));
                if !text.is_empty() {
                    message.insert("content".to_string(), json!(text));
                }
                if !reasoning.is_empty() {
                    message.insert("reasoning_content".to_string(), json!(reasoning));
                }
                if !calls.is_empty() {
                    message.insert("tool_calls".to_string(), Value::Array(calls));
                }
                messages.push(Value::Object(message));
            }
        }
    }

    let mut body = Map::new();
    body.insert("messages".to_string(), Value::Array(messages));
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
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters,
                                "strict": tool.strict,
                            },
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
                ToolChoice::Named(name) => json!({"type": "function", "function": {"name": name}}),
            },
        );
        if let Some(parallel) = request.parallel_tool_calls {
            body.insert("parallel_tool_calls".to_string(), json!(parallel));
        }
    }
    insert_sampling(&mut body, &request.sampling);
    if let Some(effort) = request.reasoning.effort.as_deref() {
        body.insert("reasoning_effort".to_string(), json!(effort));
    } else if request.reasoning.disabled {
        body.insert("reasoning_effort".to_string(), json!("none"));
    }
    body.insert("stream".to_string(), json!(request.stream));
    if request.stream {
        // Without this, several OpenAI-compatible vendors omit usage from the
        // stream and the ledger would count nothing.
        body.insert("stream_options".to_string(), json!({"include_usage": true}));
    }
    Ok(Value::Object(body))
}

fn insert_sampling(body: &mut Map<String, Value>, sampling: &SamplingParams) {
    if let Some(temperature) = sampling.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = sampling.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(max_tokens) = sampling.max_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens));
    }
    if !sampling.stop.is_empty() {
        body.insert("stop".to_string(), json!(sampling.stop));
    }
}

fn usage_from(value: &Value) -> CanonicalUsage {
    CanonicalUsage {
        input_tokens: uint_field(value, "prompt_tokens").unwrap_or_default(),
        output_tokens: uint_field(value, "completion_tokens").unwrap_or_default(),
        cached_read_tokens: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cached_write_tokens: 0,
        reasoning_tokens: value
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    }
}

pub fn render_response(response: &CanonicalResponse) -> Value {
    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    let text = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    message.insert("content".to_string(), json!(text));
    let reasoning = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    if !reasoning.is_empty() {
        message.insert("reasoning_content".to_string(), json!(reasoning));
    }
    let calls = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(arguments)
                        .unwrap_or_else(|_| "{}".to_string()),
                },
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(calls));
    }
    json!({
        "id": response.id,
        "object": "chat.completion",
        "model": response.model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": response.stop.chat(),
        }],
        "usage": {
            "prompt_tokens": response.usage.input_tokens,
            "completion_tokens": response.usage.output_tokens,
            "total_tokens": response.usage.input_tokens
                .saturating_add(response.usage.output_tokens),
            "prompt_tokens_details": {
                "cached_tokens": response.usage.cached_read_tokens
            },
            "completion_tokens_details": {
                "reasoning_tokens": response.usage.reasoning_tokens
            },
        },
    })
}

/// Parses one whole Chat reply.
pub fn parse_response(value: &Value) -> GatewayResult<CanonicalResponse> {
    let choice = value
        .pointer("/choices/0")
        .ok_or_else(|| GatewayError::Upstream {
            status: 502,
            message: "the upstream reply carried no choice".to_string(),
        })?;
    let message = choice.get("message").unwrap_or(&Value::Null);
    let mut parts = Vec::new();
    if let Some(reasoning) = string_field(message, "reasoning_content") {
        if !reasoning.is_empty() {
            parts.push(ContentPart::Reasoning {
                text: reasoning.to_string(),
                signature: None,
            });
        }
    }
    parts.extend(parts_from_content(message.get("content"))?);
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let function = call.get("function").unwrap_or(&Value::Null);
            parts.push(ContentPart::ToolCall {
                id: string_field(call, "id").unwrap_or_default().to_string(),
                name: string_field(function, "name")
                    .unwrap_or_default()
                    .to_string(),
                arguments: arguments_value(function.get("arguments")),
            });
        }
    }
    let stop = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(StopReason::from_chat)
        .unwrap_or(StopReason::EndTurn);
    Ok(CanonicalResponse {
        id: string_field(value, "id").unwrap_or_default().to_string(),
        model: string_field(value, "model").unwrap_or_default().to_string(),
        parts,
        stop,
        usage: value
            .get("usage")
            .filter(|usage| !usage.is_null())
            .map(usage_from)
            .unwrap_or_default(),
    })
}

/// Decodes a Chat stream.
#[derive(Default)]
pub struct ChatDecoder {
    tool: ToolCallAccumulator,
    tool_index: Option<u64>,
    usage: CanonicalUsage,
    started: bool,
    model: String,
    id: String,
    finished: bool,
}

impl StreamDecoder for ChatDecoder {
    fn decode(&mut self, data: &str) -> GatewayResult<Vec<CanonicalEvent>> {
        if data.trim() == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data).map_err(|_| GatewayError::Upstream {
            status: 502,
            message: "the upstream sent a Chat event that is not JSON".to_string(),
        })?;
        let mut events = Vec::new();
        if let Some(id) = string_field(&value, "id") {
            self.id = id.to_string();
        }
        if let Some(model) = string_field(&value, "model") {
            self.model = model.to_string();
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            self.usage.merge(&usage_from(usage));
            events.push(CanonicalEvent::Usage(self.usage.clone()));
        }
        if !self.started {
            self.started = true;
            events.insert(
                0,
                CanonicalEvent::Start {
                    id: self.id.clone(),
                    model: self.model.clone(),
                    usage: CanonicalUsage::default(),
                },
            );
        }
        let Some(choices) = value.get("choices").and_then(Value::as_array) else {
            return Ok(events);
        };
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(reasoning) = string_field(delta, "reasoning_content") {
                if !reasoning.is_empty() {
                    events.push(CanonicalEvent::ReasoningDelta(reasoning.to_string()));
                }
            }
            if let Some(text) = string_field(delta, "content") {
                if !text.is_empty() {
                    events.push(CanonicalEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = uint_field(call, "index");
                    let function = call.get("function").unwrap_or(&Value::Null);
                    // The id appears only on the first fragment of a call, so a
                    // change of index is what identifies a new one.
                    let starts_new = index.is_some() && index != self.tool_index;
                    if starts_new {
                        if !self.tool.is_empty() {
                            events.push(CanonicalEvent::ToolCallEnd {
                                id: self.tool.id().to_string(),
                                arguments: self.tool.arguments(),
                            });
                            self.tool = ToolCallAccumulator::default();
                        }
                        self.tool_index = index;
                        let id = string_field(call, "id").unwrap_or_default();
                        let name = string_field(function, "name").unwrap_or_default();
                        self.tool.start(id, name);
                        events.push(CanonicalEvent::ToolCallStart {
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                    } else {
                        if let Some(id) = string_field(call, "id") {
                            self.tool.start(id, "");
                        }
                        if let Some(name) = string_field(function, "name") {
                            self.tool.start("", name);
                        }
                    }
                    if let Some(arguments) = string_field(function, "arguments") {
                        if !arguments.is_empty() {
                            self.tool.push_arguments(arguments);
                            events.push(CanonicalEvent::ToolCallArgumentsDelta {
                                id: self.tool.id().to_string(),
                                fragment: arguments.to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(reason) = string_field(choice, "finish_reason") {
                if !self.finished {
                    if !self.tool.is_empty() {
                        events.push(CanonicalEvent::ToolCallEnd {
                            id: self.tool.id().to_string(),
                            arguments: self.tool.arguments(),
                        });
                        self.tool = ToolCallAccumulator::default();
                    }
                    self.finished = true;
                    events.push(CanonicalEvent::Stop(StopReason::from_chat(reason)));
                }
            }
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
        if !self.finished {
            self.finished = true;
            events.push(CanonicalEvent::Stop(if events.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            }));
        }
        Ok(events)
    }
}

/// Encodes a Chat stream.
pub struct ChatEncoder {
    model: String,
    id: String,
    role_sent: bool,
    usage: CanonicalUsage,
}

impl ChatEncoder {
    pub fn new(model: String) -> Self {
        Self {
            model,
            id: String::new(),
            role_sent: false,
            usage: CanonicalUsage::default(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        let choice = if let Some(finish) = finish {
            json!({"index": 0, "delta": delta, "finish_reason": finish})
        } else {
            json!({"index": 0, "delta": delta, "finish_reason": null})
        };
        sse_frame(
            None,
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "model": self.model,
                "choices": [choice],
            }),
        )
    }
}

impl StreamEncoder for ChatEncoder {
    fn encode(&mut self, event: &CanonicalEvent) -> GatewayResult<String> {
        Ok(match event {
            CanonicalEvent::Start { id, model, .. } => {
                self.id = id.clone();
                if !model.is_empty() {
                    self.model = model.clone();
                }
                self.role_sent = true;
                self.chunk(json!({"role": "assistant", "content": ""}), None)
            }
            CanonicalEvent::TextDelta(text) => self.chunk(json!({"content": text}), None),
            CanonicalEvent::ReasoningStart => String::new(),
            CanonicalEvent::ReasoningDelta(text) => {
                self.chunk(json!({"reasoning_content": text}), None)
            }
            CanonicalEvent::ReasoningSignature(_) => String::new(),
            CanonicalEvent::ToolCallStart { id, name } => self.chunk(
                json!({
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""},
                    }],
                }),
                None,
            ),
            CanonicalEvent::ToolCallArgumentsDelta { fragment, .. } => self.chunk(
                json!({
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": fragment},
                    }],
                }),
                None,
            ),
            CanonicalEvent::ToolCallEnd { .. } => String::new(),
            CanonicalEvent::Usage(usage) => {
                self.usage = usage.clone();
                if !self.role_sent {
                    return Ok(String::new());
                }
                sse_frame(
                    None,
                    &json!({
                        "id": self.id,
                        "object": "chat.completion.chunk",
                        "model": self.model,
                        "choices": [],
                        "usage": {
                            "prompt_tokens": usage.input_tokens,
                            "completion_tokens": usage.output_tokens,
                            "total_tokens": usage.input_tokens
                                .saturating_add(usage.output_tokens),
                        },
                    }),
                )
            }
            CanonicalEvent::Stop(reason) => self.chunk(json!({}), Some(reason.chat())),
            CanonicalEvent::Error { message, .. } => sse_frame(
                None,
                &json!({"error": {"message": message, "type": "upstream_error"}}),
            ),
        })
    }

    fn finish(&mut self) -> GatewayResult<String> {
        Ok("data: [DONE]\n\n".to_string())
    }

    fn keepalive(&self) -> Option<&'static str> {
        Some(": keepalive\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_tools_and_images() {
        let request = parse_request(&json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
                ]},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call-1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call-1", "content": "done"}
            ],
            "tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object"}}}],
            "tool_choice": "required",
            "temperature": 0.2,
            "max_tokens": 64
        }))
        .unwrap();
        assert_eq!(request.system.len(), 1);
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tool_choice, ToolChoice::Required);
        assert_eq!(request.sampling.temperature, Some(0.2));
        assert_eq!(request.sampling.max_tokens, Some(64));
        assert!(matches!(
            request.messages[0].parts[1],
            ContentPart::Image {
                source: ImageSource::Base64 { .. }
            }
        ));
        assert!(matches!(
            request.messages[1].parts[0],
            ContentPart::ToolCall { .. }
        ));
        assert!(matches!(
            request.messages[2].parts[0],
            ContentPart::ToolResult { .. }
        ));
    }

    #[test]
    fn builds_a_body_with_tool_results_before_user_text() {
        let request = CanonicalRequest {
            messages: vec![CanonicalMessage {
                role: Role::User,
                parts: vec![
                    ContentPart::ToolResult {
                        call_id: "call-1".into(),
                        content: vec![ContentPart::Text("ok".into())],
                        is_error: false,
                    },
                    ContentPart::Text("continue".into()),
                ],
            }],
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call-1");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "continue");
    }

    #[test]
    fn drops_an_empty_assistant_turn() {
        let request = CanonicalRequest {
            messages: vec![CanonicalMessage {
                role: Role::Assistant,
                parts: Vec::new(),
            }],
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn decodes_a_tool_call_stream_into_one_finished_call() {
        let mut decoder = ChatDecoder::default();
        let mut events = Vec::new();
        events.extend(
            decoder
                .decode(r#"{"id":"c1","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{\"path\":"}}]}}]}"#)
                .unwrap(),
        );
        events.extend(
            decoder
                .decode(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}}]}}]}"#)
                .unwrap(),
        );
        events.extend(
            decoder
                .decode(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
                .unwrap(),
        );
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
    }

    #[test]
    fn encodes_a_stream_with_usage_and_done() {
        let request = CanonicalRequest::for_model("m");
        let mut encoder = ChatEncoder::new("m".to_string());
        let mut output = String::new();
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Start {
                    id: "c1".into(),
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
                .encode(&CanonicalEvent::Usage(CanonicalUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..CanonicalUsage::default()
                }))
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Stop(StopReason::EndTurn))
                .unwrap(),
        );
        output.push_str(&encoder.finish().unwrap());
        assert!(output.contains("\"content\":\"hi\""));
        assert!(output.contains("\"prompt_tokens\":5"));
        assert!(output.contains("data: [DONE]"));
        assert_eq!(request.model, "m");
    }
}
