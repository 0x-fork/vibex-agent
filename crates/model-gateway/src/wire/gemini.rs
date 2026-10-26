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
        if let Some(text) = string_field(item, "text") {
            let part = if item
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                ContentPart::Reasoning {
                    text: text.to_string(),
                    signature: string_field(item, "thoughtSignature").map(|signature| {
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
                    media_type: string_field(inline, "mimeType")
                        .unwrap_or("image/png")
                        .to_string(),
                    data: string_field(inline, "data").unwrap_or_default().to_string(),
                },
            });
            continue;
        }
        if let Some(file) = item.get("fileData") {
            parts.push(ContentPart::Image {
                source: ImageSource::Url(
                    string_field(file, "fileUri")
                        .unwrap_or_default()
                        .to_string(),
                ),
            });
            continue;
        }
        if let Some(call) = item.get("functionCall") {
            // Gemini issues no id for a function call, so one is derived from
            // the name. `assign_call_ordinals` makes it unique afterwards: an
            // upstream rejects a turn that calls one tool twice, because both
            // syntheses produced the same `tool_call_id`.
            let name = string_field(call, "name").unwrap_or_default().to_string();
            parts.push(ContentPart::ToolCall {
                id: format!("{GEMINI_CALL_ID_PREFIX}{name}"),
                name,
                arguments: call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new())),
            });
            continue;
        }
        if let Some(response) = item.get("functionResponse") {
            // Gemini pairs a response to its call by name, so the id is the
            // name. `assign_call_ordinals` numbers repeated names on both
            // sides in the same order, which is how Gemini emits them.
            let name = string_field(response, "name").unwrap_or("tool").to_string();
            parts.push(ContentPart::ToolResult {
                call_id: format!("{GEMINI_CALL_ID_PREFIX}{name}"),
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

/// Marks a canonical id this codec synthesized from a Gemini tool name.
pub const GEMINI_CALL_ID_PREFIX: &str = "call_";

/// The Gemini tool name a canonical call id was synthesised from.
///
/// The ordinal `assign_call_ordinals` added is a gateway-internal detail: Gemini
/// pairs a call and its response by name, and sending `read__2` would name a
/// function the client never declared.
fn gemini_call_name(call_id: &str) -> &str {
    let name = call_id
        .strip_prefix(GEMINI_CALL_ID_PREFIX)
        .unwrap_or(call_id);
    name.split_once("__").map_or(name, |(name, _)| name)
}

/// Numbers repeated tool names so the canonical ids stay unique.
///
/// Gemini identifies a call by name alone, so a turn that runs the same tool
/// twice produces two identical synthesised ids. Every other protocol requires
/// per-call identity, and the upstream refuses the whole request when two calls
/// share one (`Duplicate value for 'tool_call_id' of call_read_file`). The
/// first occurrence keeps the plain name — that is what a later
/// `functionResponse` was paired with — and each repeat gains a position
/// suffix. Calls and their responses are numbered independently per name in
/// the order they appear, which is how Gemini emits them.
fn assign_call_ordinals(messages: &mut [CanonicalMessage]) {
    let mut seen_calls: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut seen_results: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for message in messages {
        for part in &mut message.parts {
            match part {
                ContentPart::ToolCall { id, name, .. } => {
                    let count = seen_calls.entry(name.clone()).or_insert(0);
                    *count += 1;
                    if *count > 1 {
                        *id = format!("{id}__{count}");
                    }
                }
                ContentPart::ToolResult { call_id, .. } => {
                    let name = call_id
                        .strip_prefix(GEMINI_CALL_ID_PREFIX)
                        .unwrap_or(call_id)
                        .to_string();
                    let count = seen_results.entry(name).or_insert(0);
                    *count += 1;
                    if *count > 1 {
                        *call_id = format!("{call_id}__{count}");
                    }
                }
                _ => {}
            }
        }
    }
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
    assign_call_ordinals(&mut messages);

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
                // Gemini 3 moved the tool schema to `parametersJsonSchema`;
                // older revisions used `parameters`. Reading only the legacy
                // key made every tool reach the upstream with an empty object,
                // so the model had no field names to follow and guessed
                // (`path` where the Agent requires `file_path`).
                parameters: declaration
                    .get("parameters")
                    .or_else(|| declaration.get("parametersJsonSchema"))
                    .map(lowercase_schema_types)
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
        // Gemini states streaming in the URL action suffix, which `gemini_call`
        // copies into the body so every protocol reaches the router with the
        // same shape. A body that names it directly is honoured too.
        stream: value
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        opaque: Vec::new(),
    })
}

/// JSON Schema keywords whose value is a schema type name.
const SCHEMA_TYPE_KEYS: [&str; 4] = ["type", "format", "x-type", "const"];

/// Rewrites a Gemini JSON Schema's type names to the lowercase spelling every
/// other protocol uses.
///
/// Gemini states schema types in upper case (`"OBJECT"`, `"STRING"`) while
/// OpenAI, Anthropic and the JSON Schema specification use lower case. Passing
/// them through made a translated tool declaration invalid — the upstream
/// answered `Invalid schema for function 'read_file': "STRING" is not valid
/// under any of the schemas listed in the 'anyOf' key` — which failed the whole
/// turn. Only `type` values are touched: property names and enum literals are
/// the caller's data and keep their case.
fn lowercase_schema_types(schema: &Value) -> Value {
    map_schema_types(schema, |name| name.to_ascii_lowercase())
}

/// The inverse of [`lowercase_schema_types`], for a schema going to Gemini.
fn uppercase_schema_types(schema: &Value) -> Value {
    map_schema_types(schema, |name| name.to_ascii_uppercase())
}

fn map_schema_types(schema: &Value, convert: impl Copy + Fn(&str) -> String) -> Value {
    match schema {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let converted = if SCHEMA_TYPE_KEYS.contains(&key.as_str()) {
                        match value {
                            Value::String(name) if is_schema_type_name(name) => {
                                Value::String(convert(name))
                            }
                            other => other.clone(),
                        }
                    } else if matches!(key.as_str(), "properties" | "patternProperties" | "$defs") {
                        // A map of arbitrary *names* to schemas: the keys are
                        // the caller's property names, so only values convert.
                        match value {
                            Value::Object(properties) => Value::Object(
                                properties
                                    .iter()
                                    .map(|(name, schema)| {
                                        (name.clone(), map_schema_types(schema, convert))
                                    })
                                    .collect(),
                            ),
                            other => other.clone(),
                        }
                    } else {
                        map_schema_types(value, convert)
                    };
                    (key.clone(), converted)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| map_schema_types(item, convert))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether a string names a schema type rather than a user value.
///
/// Only the seven primitive names and `object`/`array`/`null` are converted, so
/// a `format` such as `date-time` and an enum literal like `"DONE"` survive.
fn is_schema_type_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
    )
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
            // `assign_call_ordinals` suffixes a repeated name; Gemini knows the
            // call by name, so the suffix is stripped again here.
            let name = gemini_call_name(call_id);
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
                        "parametersJsonSchema": uppercase_schema_types(&tool.parameters),
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
                self.id = format!("gemini_{}", crate::wire::request_fingerprint(data));
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
                    let name = string_field(call, "name").unwrap_or_default().to_string();
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
            if let Some(reason) = string_field(candidate, "finishReason")
                && !self.stopped
            {
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
                if !id.is_empty()
                    && let Some((_, accumulator)) = self.open_tools.last_mut()
                {
                    accumulator.start(id, name);
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
                    && accumulator.arguments() == json!({})
                    && arguments != &json!({})
                {
                    accumulator
                        .push_arguments(&serde_json::to_string(arguments).unwrap_or_default());
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

    /// Gemini 3 publishes its tool schema under `parametersJsonSchema`.
    ///
    /// Reading only the older `parameters` key left every declaration with an
    /// empty object, so a translated tool call reached the upstream with no
    /// field names and the model guessed them — `path` where the Agent required
    /// `file_path`.
    #[test]
    fn reads_and_writes_the_gemini_3_tool_schema() {
        let request = parse_request(&json!({
            "model": "gemini-3",
            "contents": [{"role": "user", "parts": [{"text": "read it"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "read_file",
                    "description": "Read a file",
                    "parametersJsonSchema": {
                        "type": "object",
                        "properties": {"file_path": {"type": "string"}},
                        "required": ["file_path"]
                    }
                }]
            }]
        }))
        .unwrap();
        assert_eq!(request.tools.len(), 1);
        assert_eq!(
            request.tools[0]
                .parameters
                .pointer("/properties/file_path/type"),
            Some(&json!("string")),
            "{:?}",
            request.tools[0].parameters
        );

        let rebuilt = build_request(&request, "gemini-3").unwrap();
        assert_eq!(
            rebuilt.pointer("/tools/0/functionDeclarations/0/parametersJsonSchema/required/0"),
            Some(&json!("file_path")),
            "the schema must survive the round trip: {rebuilt}"
        );
        // Gemini's own wire says `OBJECT`/`STRING`; the internal form is the
        // lowercase every other protocol uses, and the outbound form is
        // uppercase again.
        assert_eq!(
            rebuilt.pointer("/tools/0/functionDeclarations/0/parametersJsonSchema/type"),
            Some(&json!("OBJECT")),
            "{rebuilt}"
        );
        assert_eq!(
            request.tools[0].parameters.get("type"),
            Some(&json!("object")),
            "{:?}",
            request.tools[0].parameters
        );
    }

    /// Gemini's upper-case schema types become the lower-case form the other
    /// protocols and the JSON Schema spec use, and back again on the way out.
    ///
    /// Forwarding `"STRING"` unchanged made the upstream reject the whole
    /// request: `Invalid schema for function 'read_file': "STRING" is not valid
    /// under any of the schemas listed in the 'anyOf' key`.
    #[test]
    fn schema_type_names_are_normalized_between_protocols() {
        let gemini_shape = json!({
            "type": "OBJECT",
            "properties": {
                "file_path": {"type": "STRING"},
                "limit": {"type": "INTEGER"},
                "tags": {"type": "ARRAY", "items": {"type": "STRING"}},
                // A literal that looks like a type name must survive.
                "mode": {"type": "STRING", "enum": ["OBJECT", "fast"]},
                "format_hint": {"type": "STRING", "format": "date-time"}
            },
            "required": ["file_path"]
        });

        let lowered = lowercase_schema_types(&gemini_shape);
        assert_eq!(lowered["type"], "object");
        assert_eq!(lowered["properties"]["file_path"]["type"], "string");
        assert_eq!(lowered["properties"]["limit"]["type"], "integer");
        assert_eq!(lowered["properties"]["tags"]["items"]["type"], "string");
        // Property names keep their case, and so do enum literals and formats.
        assert!(lowered["properties"].get("file_path").is_some());
        assert_eq!(
            lowered["properties"]["mode"]["enum"],
            json!(["OBJECT", "fast"]),
            "an enum literal is data, not a type"
        );
        assert_eq!(lowered["properties"]["format_hint"]["format"], "date-time");

        let raised = uppercase_schema_types(&lowered);
        assert_eq!(raised["type"], "OBJECT");
        assert_eq!(raised["properties"]["file_path"]["type"], "STRING");
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

    /// A turn that calls one tool twice keeps two distinct call ids.
    ///
    /// Gemini names a call but issues no id, so both calls synthesised
    /// `call_read_file`. Chat and Responses require per-call identity and the
    /// upstream refused the whole translated request with `Duplicate value for
    /// 'tool_call_id' of call_read_file in message[2]` — the turn died after
    /// the tools had already run.
    #[test]
    fn repeated_tool_names_get_distinct_call_ids() {
        let request = parse_request(&json!({
            "contents": [
                {"role": "user", "parts": [{"text": "read both"}]},
                {"role": "model", "parts": [
                    {"functionCall": {"name": "read_file", "args": {"path": "a"}}},
                    {"functionCall": {"name": "read_file", "args": {"path": "b"}}}
                ]},
                {"role": "user", "parts": [
                    {"functionResponse": {"name": "read_file", "response": {"content": "A"}}},
                    {"functionResponse": {"name": "read_file", "response": {"content": "B"}}}
                ]}
            ]
        }))
        .unwrap();

        let mut ids = Vec::new();
        for message in &request.messages {
            for part in &message.parts {
                if let ContentPart::ToolCall { id, .. } = part {
                    ids.push(id.clone());
                }
            }
        }
        assert_eq!(ids.len(), 2);
        assert_ne!(
            ids[0], ids[1],
            "two calls must not share one canonical id: {ids:?}"
        );

        let mut result_ids = Vec::new();
        for message in &request.messages {
            for part in &message.parts {
                if let ContentPart::ToolResult { call_id, .. } = part {
                    result_ids.push(call_id.clone());
                }
            }
        }
        assert_ne!(result_ids[0], result_ids[1], "{result_ids:?}");

        // Sending the turn back to Gemini must name the function the client
        // declared: neither the prefix nor the ordinal is part of its name.
        let body = build_request(&request, "gemini-3-pro").unwrap();
        let names = body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
            .filter_map(|part| {
                part.get("functionResponse")
                    .or_else(|| part.get("functionCall"))
                    .and_then(|entry| entry["name"].as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file"; 4], "{body}");
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
