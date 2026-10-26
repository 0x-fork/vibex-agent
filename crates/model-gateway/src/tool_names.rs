//! Tool-name normalization between the protocols the gateway bridges.
//!
//! The four wire protocols disagree about what a function name may contain.
//! Gemini permits `.` and `:` — its own MCP bridge rewrites only
//! `[^a-zA-Z0-9_\-.:]` — while the OpenAI protocols and Anthropic accept only
//! `[a-zA-Z0-9_-]` (and at most 64 characters). A Gemini client therefore sends
//! tool names such as `mcp_com.browser-use_browser-use_browser_click`, which is
//! a perfectly valid Gemini function name and an invalid OpenAI one. The
//! upstream rejects the whole request for it:
//!
//! ```text
//! Invalid 'tools[36].function.name': string does not match pattern.
//! Expected a string that matches the pattern '^[a-zA-Z0-9_-]+$'
//! ```
//!
//! Dropping the offending tool would take a capability away from the model, and
//! forwarding the name loses the turn. So the gateway rewrites the name into the
//! target's vocabulary and remembers what it stood for: the model calls the
//! rewritten name, and the reply is translated back to the name the client
//! knows. Without that reverse step the client reports an unknown tool.
//!
//! The rewrite is deterministic — the same request always produces the same
//! names — because a conversation resumes across sessions and across app
//! restarts, and a name that changed between turns would break its own history.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalResponse, ContentPart, ToolChoice};
use crate::wire::WireProtocol;

/// The longest function name the strict protocols accept.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Maps tool names into a target protocol's vocabulary and back.
///
/// Both directions are kept because they answer different questions: the
/// outbound map rewrites the request, the inbound map restores the reply. An
/// empty map means no name needed changing, which is the common case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolNameMap {
    original_to_wire: BTreeMap<String, String>,
    wire_to_original: BTreeMap<String, String>,
}

impl ToolNameMap {
    /// Collects every function name this request mentions.
    ///
    /// The tool declarations are the obvious source, but a continued
    /// conversation also replays earlier tool calls by name, and a name the
    /// upstream does not recognise fails the same way a declaration does.
    pub(crate) fn build(request: &CanonicalRequest) -> Self {
        let mut names: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .filter(|name| !name.trim().is_empty())
            .collect();
        for message in &request.messages {
            for part in &message.parts {
                if let ContentPart::ToolCall { name, .. } = part {
                    names.push(name.as_str());
                }
            }
        }
        if let ToolChoice::Named(name) = &request.tool_choice {
            names.push(name.as_str());
        }
        // Sorted so which name keeps the unsuffixed form does not depend on
        // declaration order: a retry must send byte-identical names, or the
        // upstream's prompt cache misses on every attempt.
        names.sort_unstable();
        names.dedup();

        let mut map = Self::default();
        let mut taken: BTreeMap<String, &str> = BTreeMap::new();
        for name in names {
            let candidate = sanitize_tool_name(name);
            if candidate == name && !taken.contains_key(&candidate) {
                taken.insert(candidate, name);
                continue;
            }
            let mut unique = candidate.clone();
            let mut suffix = 2usize;
            while taken.contains_key(&unique) {
                unique = suffixed_tool_name(&candidate, suffix);
                suffix += 1;
            }
            taken.insert(unique.clone(), name);
            map.original_to_wire
                .insert(name.to_string(), unique.clone());
            map.wire_to_original.insert(unique, name.to_string());
        }
        map
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.original_to_wire.is_empty()
    }

    /// Rewrites the tool names in one built request body.
    ///
    /// The body is walked rather than the IR because the same rewrite has to
    /// reach every place a name is spelled — the declarations, the replayed
    /// history of earlier calls, and the forced tool choice — and each protocol
    /// nests them differently. A translated call is always a freshly built
    /// body, so the walk sees the shape the encoder produced.
    pub(crate) fn rewrite_body(&self, body: &mut Value, target: WireProtocol) {
        if self.is_empty() {
            return;
        }
        match target {
            // Gemini already accepts every name this map produces, and its
            // `functionCall` echo is matched against the client's own registry.
            WireProtocol::GeminiGenerateContent => {}
            WireProtocol::ChatCompletions => {
                if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
                    for tool in tools {
                        rewrite_at(tool, &["function", "name"], self);
                    }
                }
                if let Some(choice) = body.get_mut("tool_choice") {
                    rewrite_at(choice, &["function", "name"], self);
                }
                if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
                    for message in messages {
                        let Some(calls) =
                            message.get_mut("tool_calls").and_then(Value::as_array_mut)
                        else {
                            continue;
                        };
                        for call in calls {
                            rewrite_at(call, &["function", "name"], self);
                        }
                    }
                }
            }
            WireProtocol::Responses => {
                rewrite_list(body, "tools", &["name"], self);
                if let Some(choice) = body.get_mut("tool_choice") {
                    rewrite_at(choice, &["name"], self);
                }
                if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
                    for item in items {
                        rewrite_at(item, &["name"], self);
                    }
                }
            }
            WireProtocol::AnthropicMessages => {
                rewrite_list(body, "tools", &["name"], self);
                if let Some(choice) = body.get_mut("tool_choice") {
                    rewrite_at(choice, &["name"], self);
                }
                if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
                    for message in messages {
                        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut)
                        else {
                            continue;
                        };
                        for block in blocks {
                            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                                rewrite_at(block, &["name"], self);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Restores the client's own names in a whole (non-streaming) reply.
    pub(crate) fn restore_response(&self, response: &mut CanonicalResponse) {
        if self.is_empty() {
            return;
        }
        for part in &mut response.parts {
            if let ContentPart::ToolCall { name, .. } = part
                && let Some(original) = self.wire_to_original.get(name.as_str())
            {
                *name = original.clone();
            }
        }
    }

    /// Restores the client's own name on one streamed event.
    pub(crate) fn restore_event(&self, event: &mut CanonicalEvent) {
        if self.is_empty() {
            return;
        }
        if let CanonicalEvent::ToolCallStart { name, .. } = event
            && let Some(original) = self.wire_to_original.get(name.as_str())
        {
            *name = original.clone();
        }
    }
}

/// Rewrites `container[path...]` when it currently holds a mapped name.
fn rewrite_at(container: &mut Value, path: &[&str], map: &ToolNameMap) {
    let mut cursor = container;
    for key in path {
        let Some(next) = cursor.get_mut(*key) else {
            return;
        };
        cursor = next;
    }
    let Some(name) = cursor.as_str() else {
        return;
    };
    if let Some(rewritten) = map.original_to_wire.get(name) {
        *cursor = Value::String(rewritten.clone());
    }
}

fn rewrite_list(body: &mut Value, key: &str, path: &[&str], map: &ToolNameMap) {
    let Some(items) = body.get_mut(key).and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        rewrite_at(item, path, map);
    }
}

/// A tool name the strict protocols accept.
///
/// Every byte outside `[a-zA-Z0-9_-]` becomes `_` rather than being dropped:
/// dropping would let `read.file` and `readfile` collide silently, and the
/// collision map can only disambiguate names it can still tell apart.
fn sanitize_tool_name(name: &str) -> String {
    if is_accepted_tool_name(name) {
        return name.to_string();
    }
    let mut sanitized = String::with_capacity(name.len());
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            sanitized.push(character);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        sanitized.push_str("tool");
    }
    // OpenAI rejects a name that starts with a digit; a leading underscore is
    // accepted by every one of the three strict protocols.
    if sanitized
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        sanitized.insert(0, '_');
    }
    if sanitized.len() > MAX_TOOL_NAME_LEN {
        let digest = short_digest(name);
        let keep = MAX_TOOL_NAME_LEN - digest.len() - 1;
        sanitized = format!("{}_{digest}", &sanitized[..keep]);
    }
    sanitized
}

fn is_accepted_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_LEN
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        && !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
}

fn suffixed_tool_name(candidate: &str, suffix: usize) -> String {
    let suffix = format!("_{suffix}");
    let keep = MAX_TOOL_NAME_LEN.saturating_sub(suffix.len());
    let mut truncated = candidate.to_string();
    truncated.truncate(keep);
    truncated.push_str(&suffix);
    truncated
}

/// A stable digest of the original name, used to keep long names distinct.
///
/// `DefaultHasher` is only guaranteed stable within one build, and that is
/// enough: the map is rebuilt with the same binary that produced the earlier
/// turn, and a name only has to stay stable for the conversation that grew out
/// of this request. Vendors cap names at 64 characters, so two distinct long
/// names truncated to the same prefix would otherwise collide.
fn short_digest(name: &str) -> String {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// Removes a key whose value is an empty array.
///
/// Some vendors read the presence of `tools` as "tools are supported" and
/// reject an empty list, so a list that lost every entry has to disappear
/// rather than become `[]`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CanonicalMessage, Role, ToolDefinition};
    use serde_json::json;

    fn definition(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: String::new(),
            parameters: json!({"type": "object"}),
            strict: false,
        }
    }

    fn request(names: &[&str]) -> CanonicalRequest {
        CanonicalRequest {
            tools: names.iter().map(|name| definition(name)).collect(),
            ..CanonicalRequest::for_model("m")
        }
    }

    #[test]
    fn an_accepted_name_is_left_alone() {
        let map = ToolNameMap::build(&request(&["read_file", "browser-click"]));
        assert!(map.is_empty(), "{map:?}");
    }

    /// The Gemini-shaped name that failed the real turn: Gemini's MCP bridge
    /// keeps `.` in `mcp_<server>_<tool>`, and the OpenAI upstream refuses it.
    #[test]
    fn a_dotted_name_is_rewritten_and_restored() {
        let map = ToolNameMap::build(&request(&[
            "mcp_com.browser-use_browser-use_browser_click",
            "read_file",
        ]));
        assert!(!map.is_empty());
        let mut body = json!({
            "tools": [
                {"type": "function", "function": {"name": "mcp_com.browser-use_browser-use_browser_click"}},
                {"type": "function", "function": {"name": "read_file"}},
            ],
            "messages": [{
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "mcp_com.browser-use_browser-use_browser_click", "arguments": "{}"}
                }]
            }],
        });
        map.rewrite_body(&mut body, WireProtocol::ChatCompletions);
        let rewritten = body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(rewritten, "mcp_com_browser-use_browser-use_browser_click");
        assert!(
            is_accepted_tool_name(&rewritten),
            "the rewritten name must satisfy the strict pattern: {rewritten}"
        );
        assert_eq!(body["tools"][1]["function"]["name"], "read_file");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            "mcp_com_browser-use_browser-use_browser_click"
        );

        // The reply has to name the client's own tool, not the rewritten one.
        let mut response = CanonicalResponse {
            parts: vec![ContentPart::ToolCall {
                id: "call_2".to_string(),
                name: rewritten,
                arguments: json!({}),
            }],
            ..CanonicalResponse::default()
        };
        map.restore_response(&mut response);
        assert!(matches!(
            &response.parts[0],
            ContentPart::ToolCall { name, .. }
                if name == "mcp_com.browser-use_browser-use_browser_click"
        ));

        let mut event = CanonicalEvent::ToolCallStart {
            id: "call_2".to_string(),
            name: "mcp_com_browser-use_browser-use_browser_click".to_string(),
        };
        map.restore_event(&mut event);
        assert!(matches!(
            event,
            CanonicalEvent::ToolCallStart { name, .. }
                if name == "mcp_com.browser-use_browser-use_browser_click"
        ));
    }

    #[test]
    fn names_longer_than_the_limit_are_truncated_without_colliding() {
        let long = "mcp_vibex-agent-delegation_delegate_to_agent_with_a_very_long_suffix";
        assert!(long.len() > MAX_TOOL_NAME_LEN);
        let map = ToolNameMap::build(&request(&[long, &format!("{long}_two")]));
        let first = map.original_to_wire.get(long).unwrap();
        let second = map.original_to_wire.get(&format!("{long}_two")).unwrap();
        assert!(first.len() <= MAX_TOOL_NAME_LEN, "{first}");
        assert!(second.len() <= MAX_TOOL_NAME_LEN, "{second}");
        assert_ne!(first, second, "truncation must not merge two tools");
        assert!(is_accepted_tool_name(first));
        assert!(is_accepted_tool_name(second));
    }

    #[test]
    fn two_names_that_sanitize_alike_stay_distinct() {
        let map = ToolNameMap::build(&request(&["read.file", "read:file"]));
        let first = map.original_to_wire.get("read.file").unwrap();
        let second = map.original_to_wire.get("read:file").unwrap();
        assert_ne!(first, second);
        assert_eq!(map.wire_to_original.len(), 2);
    }

    #[test]
    fn a_name_starting_with_a_digit_gains_a_leading_underscore() {
        let map = ToolNameMap::build(&request(&["2fa_prompt"]));
        assert_eq!(
            map.original_to_wire.get("2fa_prompt").unwrap(),
            "_2fa_prompt"
        );
    }

    #[test]
    fn a_gemini_target_keeps_the_client_name_verbatim() {
        let map = ToolNameMap::build(&request(&["mcp_a.b_c"]));
        let mut body = json!({
            "tools": [{"functionDeclarations": [{"name": "mcp_a.b_c"}]}],
        });
        map.rewrite_body(&mut body, WireProtocol::GeminiGenerateContent);
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "mcp_a.b_c"
        );
    }

    #[test]
    fn history_names_are_rewritten_for_every_strict_protocol() {
        let name = "mcp_a.b_c";
        let map = ToolNameMap::build(&request(&[name]));
        let rewritten = map.original_to_wire.get(name).unwrap().clone();

        let mut messages = CanonicalMessage {
            role: Role::Assistant,
            parts: vec![ContentPart::ToolCall {
                id: "call_1".to_string(),
                name: name.to_string(),
                arguments: json!({}),
            }],
        };
        messages.parts.push(ContentPart::Text("done".to_string()));

        let mut anthropic = json!({
            "tools": [{"name": name}],
            "tool_choice": {"type": "tool", "name": name},
            "messages": [{"role": "assistant", "content": [
                {"type": "tool_use", "id": "call_1", "name": name, "input": {}},
            ]}],
        });
        map.rewrite_body(&mut anthropic, WireProtocol::AnthropicMessages);
        assert_eq!(anthropic["tools"][0]["name"], rewritten);
        assert_eq!(anthropic["tool_choice"]["name"], rewritten);
        assert_eq!(anthropic["messages"][0]["content"][0]["name"], rewritten);

        let mut responses = json!({
            "tools": [{"type": "function", "name": name}],
            "input": [{"type": "function_call", "name": name, "call_id": "call_1"}],
        });
        map.rewrite_body(&mut responses, WireProtocol::Responses);
        assert_eq!(responses["tools"][0]["name"], rewritten);
        assert_eq!(responses["input"][0]["name"], rewritten);
    }
}
