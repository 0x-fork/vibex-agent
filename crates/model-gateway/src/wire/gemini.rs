//! Google Gemini `generateContent`.

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

fn parts(value: Option<&Value>) -> GatewayResult<Vec<ContentPart>> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        if let Some(text) = string_field(&item, "text") {
            let part = if item
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                ContentPart::Reasoning {
                    text: text.to_string(),
                    signature: string_field(&item, "thoughtSignature").map(|signature| {
                        crate::ir::OpaqueSignature {
                            protocol: vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string(),
                            value: signature.to_string(),
                        }
                    }),
                }
            } else {
                ContentPart::Text(text.to_string())
            };
            parts.push(part);
            continue;
        }
        if let Some(inline) = item.get("inlineData") {
            parts.push(ContentPart::Image {
                source: ImageSource::Base64 {
                    media_type: string_field(&inline, "mimeType")
                        .unwrap_or("image/png")
                        .to_string(),
                    data: string_field(&inline, "data")
                        .unwrap_or_default()
                        .to_string(),
                },
            });
            continue;
        }
        if let Some(file) = item.get("fileData") {
            parts.push(ContentPart::Image {
                source: ImageSource::Url(
                    string_field(&file, "fileUri")
                        .unwrap_or_default()
                        .to_string(),
                ),
            });
            continue;
        }
        if let Some(call) = item.get("functionCall") {
            parts.push(ContentPart::ToolCall {
                // Gemini does not issue ids for function calls. A deterministic
                // id keeps a later `functionResponse` paired with its call;
                // the name is unique within one reply because Gemini rejects a
                // duplicate tool set.
                id: format!("call_{}", string_field(&call, "name").unwrap_or("tool")),
                name: string_field(&call, "name").unwrap_or_default().to_string(),
                arguments: call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new())),
            });
            continue;
        }
        if let Some(response) = item.get("functionResponse") {
            parts.push(ContentPart::ToolResult {
                call_id: format!("call_{}", string_field(&response, "name").unwrap_or("tool")),
                content: vec![ContentPart::Text(
                    serde_json::to_string(response.get("response").unwrap_or(&Value::Null))
                        .unwrap_or_default(),
                )],
                is_error: false,
            });
            continue;
        }
        // An unknown part is left out rather than failing the whole request:
        // Gemini adds part kinds over time and none of them affect routing.
    }
    Ok(parts)
}

pub fn parse_request(value: &Value) -> GatewayResult<CanonicalRequest> {
    let body = object(value, "a Gemini request")?;
    let system = parts(value.pointer("/systemInstruction/parts"))?;
    let mut messages = Vec::new();
    if let Some(contents) = body.get("contents").and_then(Value::as_array) {
        for content in contents {
            // Gemini omits `role` for a user turn.
            let role = match string_field(content, "role") {
                Some("model") => Role::Assistant,
                _ => Role::User,
            };
            messages.push(CanonicalMessage {
                role,
                parts: parts(content.get("parts"))?,
            });
        }
    }

    let mut tools = Vec::new();
    if let Some(declarations) = value
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
    {
        for declaration in declarations {
            tools.push(ToolDefinition {
                name: string_field(declaration, "name")
                    .unwrap_or_default()
                    .to_string(),
                description: string_field(declaration, "description")
                    .unwrap_or_default()
                    .to_string(),
                parameters: declaration
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
                strict: false,
            });
        }
    }

    let tool_choice = match value.pointer("/toolConfig/functionCallingConfig/mode") {
        Some(Value::String(mode)) if mode == "NONE" => ToolChoice::None,
        Some(Value::String(mode)) if mode == "ANY" => {
            let named = value
                .pointer("/toolConfig/functionCallingConfig/allowedFunctionNames/0")
                .and_then(Value::as_str);
            match named {
                Some(name) => ToolChoice::Named(name.to_string()),
                None => ToolChoice::Required,
            }
        }
        _ => ToolChoice::Auto,
    };

    let generation = body.get("generationConfig").unwrap_or(&Value::Null);
    Ok(CanonicalRequest {
        model: string_field(value, "model").unwrap_or_default().to_string(),
        system,
        messages,
        tools,
        tool_choice,
        parallel_tool_calls: None,
        sampling: SamplingParams {
            temperature: number_field(generation, "temperature"),
            top_p: number_field(generation, "topP"),
            max_tokens: uint_field(generation, "maxOutputTokens"),
            stop: generation
                .get("stopSequences")
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
        reasoning: ReasoningRequest {
            disabled: matches!(
                generation.pointer("/thinkingConfig/thinkingBudget"),
                Some(Value::Number(_)) if number_field(generation, "thinkingBudget") == Some(0.0)
            ),
            effort: None,
        },
        stream: false,
        opaque: Vec::new(),
    })
}

fn part_value(part: &ContentPart) -> GatewayResult<Option<Value>> {
    let value = match part {
        ContentPart::Text(text) if !text.is_empty() => Some(json!({"text": text})),
        ContentPart::Reasoning { text, signature } => Some(match signature {
            Some(signature)
                if signature.protocol == vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI =>
            {
                json!({
                    "text": text,
                    "thought": true,
                    "thoughtSignature": signature.value,
                })
            }
            _ => json!({"text": text, "thought": true}),
        }),
        ContentPart::Image { source } => Some(match source {
            ImageSource::Base64 { media_type, data } => json!({
                "inlineData": {"mimeType": media_type, "data": data},
            }),
            ImageSource::Url(url) => json!({
                "fileData": {"mimeType": "image/png", "fileUri": url},
            }),
            ImageSource::VendorFileId(_) => return Err(GatewayError::UnsupportedImageReference),
        }),
        ContentPart::ToolCall {
            name, arguments, ..
        } => Some(json!({
            "functionCall": {"name": name, "args": arguments},
        })),
        ContentPart::ToolResult {
            call_id, content, ..
        } => {
            let name = call_id.strip_prefix("call_").unwrap_or(call_id);
            let text = content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            let response = serde_json::from_str(&text).unwrap_or_else(|_| json!({"result": text}));
            Some(json!({
                "functionResponse": {"name": name, "response": response},
            }))
        }
        _ => None,
    };
    Ok(value)
}

pub fn build_request(request: &CanonicalRequest, _model: &str) -> GatewayResult<Value> {
    let mut body = Map::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        let parts = message
            .parts
            .iter()
            .map(part_value)
            .collect::<GatewayResult<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "model",
            },
            "parts": parts,
        }));
    }
    body.insert("contents".to_string(), Value::Array(contents));
    if !request.system.is_empty() {
        let parts = request
            .system
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(json!({"text": text})),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            body.insert("systemInstruction".to_string(), json!({"parts": parts}));
        }
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            json!([{
                "functionDeclarations": request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }))
                    .collect::<Vec<_>>(),
            }]),
        );
        let mode = match &request.tool_choice {
            ToolChoice::Auto => Some(json!({"mode": "AUTO"})),
            ToolChoice::None => Some(json!({"mode": "NONE"})),
            ToolChoice::Required => Some(json!({"mode": "ANY"})),
            ToolChoice::Named(name) => Some(json!({
                "mode": "ANY",
                "allowedFunctionNames": [name],
            })),
        };
        if let Some(mode) = mode {
            body.insert(
                "toolConfig".to_string(),
                json!({"functionCallingConfig": mode}),
            );
        }
    }
    let mut generation = Map::new();
    if let Some(temperature) = request.sampling.temperature {
        generation.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        generation.insert("topP".to_string(), json!(top_p));
    }
    if let Some(max_tokens) = request.sampling.max_tokens {
        generation.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if !request.sampling.stop.is_empty() {
        generation.insert("stopSequences".to_string(), json!(request.sampling.stop));
    }
    if request.reasoning.disabled {
        generation.insert("thinkingConfig".to_string(), json!({"thinkingBudget": 0}));
    }
    if !generation.is_empty() {
        body.insert("generationConfig".to_string(), Value::Object(generation));
    }
    Ok(Value::Object(body))
}

fn usage_from(value: &Value) -> CanonicalUsage {
    CanonicalUsage {
        input_tokens: uint_field(value, "promptTokenCount").unwrap_or_default(),
        output_tokens: uint_field(value, "candidatesTokenCount").unwrap_or_default(),
        cached_read_tokens: uint_field(value, "cachedContentTokenCount").unwrap_or_default(),
        cached_write_tokens: 0,
        reasoning_tokens: uint_field(value, "thoughtsTokenCount").unwrap_or_default(),
    }
}

pub fn render_response(response: &CanonicalResponse) -> Value {
    let parts = response
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) if !text.is_empty() => Some(json!({"text": text})),
            ContentPart::Reasoning { text, .. } => Some(json!({"text": text, "thought": true})),
            ContentPart::ToolCall {
                name, arguments, ..
            } => Some(json!({
                "functionCall": {"name": name, "args": arguments},
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": response.stop.gemini(),
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": response.usage.input_tokens,
            "candidatesTokenCount": response.usage.output_tokens,
            "cachedContentTokenCount": response.usage.cached_read_tokens,
            "thoughtsTokenCount": response.usage.reasoning_tokens,
            "totalTokenCount": response.usage.total(),
        },
        "modelVersion": response.model,
    })
}

/// Parses one whole Gemini reply.
pub fn parse_response(value: &Value) -> GatewayResult<CanonicalResponse> {
    let parts = parts(value.pointer("/candidates/0/content/parts"))?;
    let stop = value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
        .map(StopReason::from_gemini)
        .unwrap_or(StopReason::EndTurn);
    let usage = value
        .get("usageMetadata")
        .map(usage_from)
        .unwrap_or_default();
    Ok(CanonicalResponse {
        id: String::new(),
        model: string_field(value, "modelVersion")
            .unwrap_or_default()
            .to_string(),
        parts,
        stop,
        usage,
    })
}

#[derive(Default)]
pub struct GeminiDecoder {
    tool: ToolCallAccumulator,
    usage: CanonicalUsage,
    started: bool,
    id: String,
    model: String,
    stopped: bool,
    /// Set once a function call has been seen. Gemini reports `finishReason`
    /// `STOP` even when it asked for tools, so the decoder has to remember.
    saw_tool_call: bool,
}

impl StreamDecoder for GeminiDecoder {
    fn decode(&mut self, data: &str) -> GatewayResult<Vec<CanonicalEvent>> {
        let value: Value = serde_json::from_str(data).map_err(|_| GatewayError::Upstream {
            status: 502,
            message: "the upstream sent a Gemini event that is not JSON".to_string(),
        })?;
        let mut events = Vec::new();
        if let Some(usage) = value.get("usageMetadata") {
            self.usage.merge(&usage_from(usage));
            events.push(CanonicalEvent::Usage(self.usage.clone()));
        }
        if let Some(model) = string_field(&value, "modelVersion") {
            self.model = model.to_string();
        }
        if !self.started {
            self.started = true;
            if self.id.is_empty() {
                self.id = format!("gemini_{}", &crate::wire::request_fingerprint(data));
            }
            events.insert(
                0,
                CanonicalEvent::Start {
                    id: self.id.clone(),
                    model: self.model.clone(),
                    usage: CanonicalUsage::default(),
                },
            );
        }
        let Some(candidates) = value.get("candidates").and_then(Value::as_array) else {
            return Ok(events);
        };
        for candidate in candidates {
            let parts = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for part in parts {
                if let Some(call) = part.get("functionCall") {
                    let name = string_field(&call, "name").unwrap_or_default().to_string();
                    let id = format!("call_{name}");
                    self.tool.start(&id, &name);
                    self.saw_tool_call = true;
                    events.push(CanonicalEvent::ToolCallStart {
                        id: id.clone(),
                        name,
                    });
                    // Gemini gives the complete argument object at once; it has
                    // to be re-split by the encoder for protocols that stream
                    // fragments.
                    let arguments = call.get("args").cloned().unwrap_or(Value::Null);
                    let fragment = serde_json::to_string(&arguments).unwrap_or_default();
                    self.tool.push_arguments(&fragment);
                    events.push(CanonicalEvent::ToolCallArgumentsDelta { id, fragment });
                    continue;
                }
                let Some(text) = string_field(&part, "text") else {
                    continue;
                };
                if part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    events.push(CanonicalEvent::ReasoningDelta(text.to_string()));
                } else {
                    events.push(CanonicalEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(reason) = string_field(candidate, "finishReason") {
                if !self.stopped {
                    self.stopped = true;
                    if !self.tool.is_empty() {
                        events.push(CanonicalEvent::ToolCallEnd {
                            id: self.tool.id().to_string(),
                            arguments: self.tool.arguments(),
                        });
                        self.tool = ToolCallAccumulator::default();
                    }
                    let stop = StopReason::from_gemini(reason);
                    events.push(CanonicalEvent::Stop(
                        if stop == StopReason::EndTurn && self.saw_tool_call {
                            StopReason::ToolUse
                        } else {
                            stop
                        },
                    ));
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
        if !self.stopped {
            self.stopped = true;
            events.push(CanonicalEvent::Stop(if self.saw_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }));
        }
        Ok(events)
    }
}

pub struct GeminiEncoder {
    model: String,
    open_tools: Vec<(String, ToolCallAccumulator)>,
    usage: CanonicalUsage,
    finished: bool,
}

impl GeminiEncoder {
    pub fn new(model: String) -> Self {
        Self {
            model,
            open_tools: Vec::new(),
            usage: CanonicalUsage::default(),
            finished: false,
        }
    }

    fn flush_tools(&mut self) -> String {
        if self.open_tools.is_empty() {
            return String::new();
        }
        let calls = self
            .open_tools
            .drain(..)
            .map(|(name, accumulator)| {
                json!({
                    "functionCall": {"name": name, "args": accumulator.arguments()},
                })
            })
            .collect::<Vec<_>>();
        sse_frame(
            None,
            &json!({
                "candidates": [{"content": {"role": "model", "parts": calls}, "index": 0}],
            }),
        )
    }
}

impl StreamEncoder for GeminiEncoder {
    fn encode(&mut self, event: &CanonicalEvent) -> GatewayResult<String> {
        Ok(match event {
            CanonicalEvent::Start { model, .. } => {
                if !model.is_empty() {
                    self.model = model.clone();
                }
                String::new()
            }
            CanonicalEvent::TextDelta(text) => {
                let mut output = self.flush_tools();
                output.push_str(&sse_frame(
                    None,
                    &json!({
                        "candidates": [{
                            "content": {"role": "model", "parts": [{"text": text}]},
                            "index": 0,
                        }],
                    }),
                ));
                output
            }
            CanonicalEvent::ReasoningStart => String::new(),
            CanonicalEvent::ReasoningDelta(text) => {
                let mut output = self.flush_tools();
                output.push_str(&sse_frame(
                    None,
                    &json!({
                        "candidates": [{
                            "content": {
                                "role": "model",
                                "parts": [{"text": text, "thought": true}],
                            },
                            "index": 0,
                        }],
                    }),
                ));
                output
            }
            CanonicalEvent::ReasoningSignature(_) => String::new(),
            CanonicalEvent::ToolCallStart { id, name } => {
                // Gemini batches calls into one content block, so a new call
                // does not flush the one before it.
                self.open_tools
                    .push((name.clone(), ToolCallAccumulator::default()));
                if !id.is_empty() {
                    if let Some((_, accumulator)) = self.open_tools.last_mut() {
                        accumulator.start(id, name);
                    }
                }
                String::new()
            }
            CanonicalEvent::ToolCallArgumentsDelta { fragment, .. } => {
                if let Some((_, accumulator)) = self.open_tools.last_mut() {
                    accumulator.push_arguments(fragment);
                }
                String::new()
            }
            CanonicalEvent::ToolCallEnd { id, arguments } => {
                if let Some((_, accumulator)) = self
                    .open_tools
                    .iter_mut()
                    .find(|(_, accumulator)| accumulator.id() == id)
                {
                    if accumulator.arguments() == json!({}) && arguments != &json!({}) {
                        accumulator
                            .push_arguments(&serde_json::to_string(arguments).unwrap_or_default());
                    }
                }
                String::new()
            }
            CanonicalEvent::Usage(usage) => {
                self.usage = usage.clone();
                String::new()
            }
            CanonicalEvent::Stop(reason) => {
                let mut output = self.flush_tools();
                output.push_str(&sse_frame(
                    None,
                    &json!({
                        "candidates": [{
                            "content": {"role": "model", "parts": []},
                            "finishReason": reason.gemini(),
                            "index": 0,
                        }],
                        "usageMetadata": {
                            "promptTokenCount": self.usage.input_tokens,
                            "candidatesTokenCount": self.usage.output_tokens,
                            "cachedContentTokenCount": self.usage.cached_read_tokens,
                            "thoughtsTokenCount": self.usage.reasoning_tokens,
                            "totalTokenCount": self.usage.total(),
                        },
                        "modelVersion": self.model,
                    }),
                ));
                self.finished = true;
                output
            }
            CanonicalEvent::Error { message, .. } => sse_frame(
                None,
                &json!({"error": {"code": 502, "message": message, "status": "INTERNAL"}}),
            ),
        })
    }

    fn finish(&mut self) -> GatewayResult<String> {
        if self.finished {
            return Ok(String::new());
        }
        let mut output = self.flush_tools();
        output.push_str(&sse_frame(
            None,
            &json!({
                "candidates": [{
                    "content": {"role": "model", "parts": []},
                    "finishReason": "STOP",
                    "index": 0,
                }],
                "usageMetadata": {
                    "promptTokenCount": self.usage.input_tokens,
                    "candidatesTokenCount": self.usage.output_tokens,
                    "totalTokenCount": self.usage.total(),
                },
                "modelVersion": self.model,
            }),
        ));
        Ok(output)
    }

    fn keepalive(&self) -> Option<&'static str> {
        // Google's SDK reads `data:` lines strictly and stalls on a comment.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_contents_system_and_function_parts() {
        let request = parse_request(&json!({
            "model": "gemini-2.5-pro",
            "systemInstruction": {"parts": [{"text": "brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "read"}, {"inlineData": {"mimeType": "image/png", "data": "AAA"}}]},
                {"role": "model", "parts": [{"functionCall": {"name": "read", "args": {"path": "a"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "read", "response": {"ok": true}}}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "read", "parameters": {"type": "object"}}]}],
            "generationConfig": {"temperature": 0.1, "maxOutputTokens": 32}
        }))
        .unwrap();
        assert_eq!(request.system.len(), 1);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.sampling.max_tokens, Some(32));
        assert!(matches!(
            request.messages[1].parts[0],
            ContentPart::ToolCall { ref name, .. } if name == "read"
        ));
        assert!(matches!(
            request.messages[2].parts[0],
            ContentPart::ToolResult { .. }
        ));
    }

    #[test]
    fn builds_parts_and_tool_config() {
        let request = CanonicalRequest {
            system: vec![ContentPart::Text("brief".into())],
            messages: vec![CanonicalMessage {
                role: Role::Assistant,
                parts: vec![ContentPart::ToolCall {
                    id: "call_read".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                }],
            }],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                parameters: json!({"type": "object"}),
                strict: false,
            }],
            tool_choice: ToolChoice::Required,
            ..CanonicalRequest::for_model("m")
        };
        let body = build_request(&request, "m").unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "brief");
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "read");
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            body["contents"][0]["parts"][0]["functionCall"]["args"]["path"],
            "a"
        );
    }

    #[test]
    fn decodes_whole_function_calls_and_finishes_with_tool_use() {
        let mut decoder = GeminiDecoder::default();
        let mut events = Vec::new();
        for frame in [
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hm","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"path":"a"}}}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}}"#,
        ] {
            events.extend(decoder.decode(frame).unwrap());
        }
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalEvent::ReasoningDelta(text) if text == "hm"
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
    }

    #[test]
    fn encoder_batches_tool_calls_into_one_content_block() {
        let mut encoder = GeminiEncoder::new("m".into());
        let mut output = String::new();
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::ToolCallStart {
                    id: "call_a".into(),
                    name: "a".into(),
                })
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::ToolCallArgumentsDelta {
                    id: "call_a".into(),
                    fragment: "{\"x\":1}".into(),
                })
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::ToolCallEnd {
                    id: "call_a".into(),
                    arguments: json!({"x": 1}),
                })
                .unwrap(),
        );
        output.push_str(
            &encoder
                .encode(&CanonicalEvent::Stop(StopReason::ToolUse))
                .unwrap(),
        );
        assert_eq!(output.matches("functionCall").count(), 1);
        assert!(output.contains("\"args\":{\"x\":1}"));
        assert!(output.contains("MAX_TOKENS") || output.contains("finishReason"));
    }

    #[test]
    fn gemini_has_no_keepalive() {
        assert!(GeminiEncoder::new("m".into()).keepalive().is_none());
    }
}
