//! Upstream and Key selection.
//!
//! This module is pure: it takes the state the service read, returns the order
//! to try candidates in, and says why. That keeps the interesting properties —
//! a cooling Key is never tried first, a pinned conversation stays put, a
//! rotating pool advances exactly once per request — testable without a
//! listener or a database.
//!
//! Two invariants matter more than load balance:
//!
//! - A Key that failed authentication is never selected again on its own.
//! - A reply that has already reached the client is never retried, so the
//!   caller only re-plans while [`RoutePlan::may_fail_over`] is true.

use sha2::{Digest, Sha256};

use vibex_core::{
    GatewayFailureKind, GatewayModelRef, GatewayRoutingStrategy, GatewayTokenLimit,
    ModelProviderProfileId, RequestId,
};

/// One Key as selection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateKey {
    pub profile: ModelProviderProfileId,
    pub credential: RequestId,
    /// Lower sorts first within a strategy's own ordering.
    pub priority: i64,
    /// The user asked for this Key first. It leads every strategy, and the
    /// strategy orders the rest.
    pub preferred: bool,
    /// The Key may answer right now.
    pub ready: bool,
    /// Why it is not ready, when it is not.
    pub cooling: Option<GatewayFailureKind>,
    /// Tokens observed in the current ceiling window.
    pub used_tokens: u64,
    /// The local ceiling for the owning Provider, when one is set.
    pub token_limit: Option<GatewayTokenLimit>,
    /// Tokens already used in that ceiling's window.
    pub window_used_tokens: Option<u64>,
}

impl CandidateKey {
    pub fn new(profile: ModelProviderProfileId, credential: RequestId, priority: i64) -> Self {
        Self {
            profile,
            credential,
            priority,
            preferred: false,
            ready: true,
            cooling: None,
            used_tokens: 0,
            token_limit: None,
            window_used_tokens: None,
        }
    }

    /// Marks this Key as the user's preferred first attempt.
    pub fn preferred(mut self) -> Self {
        self.preferred = true;
        self
    }

    pub fn cooling(mut self, kind: GatewayFailureKind) -> Self {
        self.ready = false;
        self.cooling = Some(kind);
        self
    }

    pub fn with_usage(mut self, used_tokens: u64) -> Self {
        self.used_tokens = used_tokens;
        self
    }

    pub fn with_token_limit(mut self, limit: GatewayTokenLimit, used: u64) -> Self {
        self.token_limit = Some(limit);
        self.window_used_tokens = Some(used);
        self
    }

    /// True when the Provider's locally observed tokens have reached the
    /// ceiling the user set.
    pub fn over_token_limit(&self) -> bool {
        match (self.token_limit, self.window_used_tokens) {
            (Some(limit), Some(used)) => used >= limit.tokens,
            _ => false,
        }
    }

    /// The identity a conversation pins to.
    pub fn key(&self) -> (&ModelProviderProfileId, &RequestId) {
        (&self.profile, &self.credential)
    }
}

/// What the service asks selection to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    pub candidates: Vec<CandidateKey>,
    pub strategy: GatewayRoutingStrategy,
    /// A monotonically increasing per-provider counter, used by rotation.
    pub rotation: u64,
    /// A Key a conversation already chose.
    pub pinned: Option<(ModelProviderProfileId, RequestId)>,
}

/// The attempt order and its explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    /// Ready candidates in attempt order, then the cooling ones, so a final
    /// failure can report the real reason instead of a bare timeout.
    pub order: Vec<CandidateKey>,
    pub skipped_cooling: usize,
    pub skipped_over_limit: usize,
    pub pinned: bool,
}

impl RoutePlan {
    /// The candidates a request may actually be sent to.
    pub fn attemptable(&self) -> impl Iterator<Item = &CandidateKey> {
        self.order
            .iter()
            .take(self.order.len().saturating_sub(self.skipped_cooling))
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The soonest a cooling candidate recovers, for a `Retry-After` hint.
    pub fn retry_after_hint(&self) -> Option<GatewayFailureKind> {
        self.order
            .iter()
            .rev()
            .find_map(|candidate| candidate.cooling)
    }

    /// Whether a failure on the first attempt may be retried on the next
    /// candidate. Authentication failures never fail over: the credential is
    /// wrong for every request until it is replaced.
    pub fn may_fail_over(&self) -> bool {
        self.attemptable().count() > 1
    }
}

/// Orders candidates for one request.
pub fn plan_route(request: RouteRequest) -> RoutePlan {
    let RouteRequest {
        mut candidates,
        strategy,
        rotation,
        pinned,
    } = request;

    // A pinned Key wins whenever it can still answer. Re-picking on every turn
    // would throw away the vendor's prompt cache, which is the single largest
    // cost and latency factor for a long conversation.
    let mut pinned_hit = false;
    if let Some((profile, credential)) = pinned.as_ref()
        && let Some(index) = candidates.iter().position(|candidate| {
            &candidate.profile == profile && &candidate.credential == credential
        })
    {
        let candidate = &candidates[index];
        if candidate.ready && !candidate.over_token_limit() {
            let candidate = candidates.remove(index);
            candidates.insert(0, candidate);
            pinned_hit = true;
        }
    }

    let pinned_matches = |candidate: &CandidateKey| {
        pinned.as_ref().is_some_and(|(profile, credential)| {
            &candidate.profile == profile && &candidate.credential == credential
        })
    };
    let mut ready = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_over_limit = 0;
    for candidate in candidates {
        if candidate.ready && !candidate.over_token_limit() {
            ready.push(candidate);
        } else {
            if candidate.ready {
                skipped_over_limit += 1;
            }
            skipped.push(candidate);
        }
    }
    let skipped_cooling = skipped.len();

    if pinned_hit {
        // The pin already sits at the front of `ready`; the rest keep their
        // configured order so a second attempt does not jump around.
        ready.sort_by_key(|candidate| candidate.priority);
        if let Some(index) = ready.iter().position(pinned_matches) {
            let pinned_candidate = ready.remove(index);
            ready.insert(0, pinned_candidate);
        }
    } else {
        sort_ready(&mut ready, strategy, rotation);
    }

    let mut order = ready;
    order.extend(skipped);
    RoutePlan {
        order,
        skipped_cooling,
        skipped_over_limit,
        pinned: pinned_hit,
    }
}

fn sort_ready(ready: &mut [CandidateKey], strategy: GatewayRoutingStrategy, rotation: u64) {
    // A preferred Key is the user's explicit choice of what to try first. That
    // holds under every strategy: rotation and usage decide the rest of the
    // pool, they do not overrule the one Key the user asked for.
    ready.sort_by_key(|candidate| (!candidate.preferred, candidate.priority));
    let preferred = ready
        .iter()
        .take_while(|candidate| candidate.preferred)
        .count();
    let rest = &mut ready[preferred..];
    match strategy {
        GatewayRoutingStrategy::Ordered => {}
        GatewayRoutingStrategy::RoundRobin => {
            if !rest.is_empty() {
                let offset = (rotation as usize) % rest.len();
                rest.rotate_left(offset);
            }
        }
        GatewayRoutingStrategy::LeastUsed => {
            rest.sort_by(|left, right| {
                left.used_tokens
                    .cmp(&right.used_tokens)
                    .then(left.priority.cmp(&right.priority))
            });
        }
    }
}

/// Classifies an upstream failure into the cooldown vocabulary.
///
/// The status is authoritative; the body only refines a 429 or a 402, because
/// vendors use the same status for "slow down" and "your plan is spent" and
/// the two need different cooldowns.
pub fn classify_failure(status: u16, body: &str) -> GatewayFailureKind {
    let words = body.to_lowercase();
    let mentions_credit = [
        "insufficient_quota",
        "insufficient balance",
        "insufficient_credit",
        "credit balance",
        "billing",
        "payment required",
        "out of credit",
        "balance is",
        "余额",
        "欠费",
    ]
    .iter()
    .any(|needle| words.contains(needle));
    let mentions_quota = [
        "quota",
        "usage limit",
        "usage_limit",
        "limit reached",
        "exceeded your",
        "plan limit",
        "out of budget",
        "额度",
        "上限",
    ]
    .iter()
    .any(|needle| words.contains(needle));
    let mentions_rate = [
        "rate limit",
        "rate_limit",
        "too many requests",
        "requests per",
        "tokens per minute",
        "slow down",
        "限流",
        "频率",
    ]
    .iter()
    .any(|needle| words.contains(needle));

    match status {
        401 => GatewayFailureKind::Auth,
        402 => {
            if mentions_quota && !mentions_credit {
                GatewayFailureKind::Quota
            } else {
                GatewayFailureKind::Credit
            }
        }
        403 => {
            // A vendor also uses 403 for a region or policy refusal, which is
            // not a credential problem. Only an explicit auth word counts.
            if words.contains("api key")
                || words.contains("unauthorized")
                || words.contains("authentication")
                || words.contains("invalid key")
            {
                GatewayFailureKind::Auth
            } else {
                GatewayFailureKind::Other
            }
        }
        404 => GatewayFailureKind::Other,
        408 | 504 => GatewayFailureKind::Upstream,
        429 => {
            if mentions_credit {
                GatewayFailureKind::Credit
            } else if mentions_quota && !mentions_rate {
                GatewayFailureKind::Quota
            } else {
                GatewayFailureKind::Rate
            }
        }
        500..=599 => GatewayFailureKind::Upstream,
        _ => GatewayFailureKind::Other,
    }
}

/// Whether a failed upstream call failed because the *shape* of the request did
/// not match the protocol the endpoint speaks.
///
/// This is the one failure the Key has nothing to do with. A vendor that
/// answers `422 unknown variant` is telling the gateway that this body belongs
/// to a different API, so cooling the Key would blame the credential for a
/// routing mistake — and, with a single Key, would leave every later request
/// refused before it could be retried on the right protocol.
pub fn is_protocol_mismatch(status: u16, body: &str) -> bool {
    let words = body.to_lowercase();
    let shape_words = [
        "unknown variant",
        "unknown field",
        "missing field",
        "unexpected field",
        "failed to deserialize",
        "invalid type",
        "expected one of",
        "unsupported",
        "not supported",
        "no such",
    ]
    .iter()
    .any(|needle| words.contains(needle));
    match status {
        // Wrong media type or wrong request schema. A 404 is deliberately not
        // here: it also means "this address is wrong", which no other protocol
        // would fix. The caller treats it as a shape problem only when it has
        // another protocol left to try, because then a path that does not exist
        // under this API is a real possibility.
        415 | 422 => true,
        // A 400 is only the body's shape when the vendor says so; a plain 400
        // can also be a content refusal, which is the request's fault and must
        // keep its ordinary treatment.
        400 => shape_words,
        _ => false,
    }
}

/// The health a Key should be recorded with after one failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyHealth {
    Ready,
    Cooling {
        kind: GatewayFailureKind,
        until_ms: i64,
    },
    Invalid,
}

impl KeyHealth {
    pub fn from_failure(kind: GatewayFailureKind, now_ms: i64) -> Self {
        match kind.cooldown_ms() {
            Some(duration) => Self::Cooling {
                kind,
                until_ms: now_ms.saturating_add(duration),
            },
            None => Self::Invalid,
        }
    }
}

/// One upstream as model resolution sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelNamespace {
    pub profile: ModelProviderProfileId,
    /// The short, human-writable prefix a model id may use instead of the
    /// long profile id.
    pub alias: String,
    pub models: Vec<String>,
}

/// Splits a model id into its Provider prefix and model id, and reports whether
/// the bare id is ambiguous among the configured upstreams.
///
/// A prefix may be either the full Provider Profile id or the upstream's short
/// alias, so an Agent can be pointed at `deepseek/deepseek-chat` without
/// spelling out an internal id. An alias that matches two upstreams is treated
/// as no match rather than guessed.
pub fn resolve_model<'a>(requested: &'a str, namespaces: &[ModelNamespace]) -> ResolvedModel<'a> {
    let parsed = GatewayModelRef::parse(requested);
    if let Some(prefix) = parsed.provider {
        let matches = namespaces
            .iter()
            .filter(|namespace| namespace.profile.as_str() == prefix || namespace.alias == prefix)
            .collect::<Vec<_>>();
        return match matches.len() {
            1 => ResolvedModel::Qualified {
                profile: matches[0].profile.clone(),
                model: parsed.model,
            },
            _ => ResolvedModel::NotFound {
                requested: requested.to_string(),
                closest: closest_models(requested, namespaces),
            },
        };
    }
    let matches = namespaces
        .iter()
        .filter(|namespace| namespace.models.iter().any(|model| model == parsed.model))
        .map(|namespace| namespace.profile.clone())
        .collect::<Vec<_>>();
    match matches.len() {
        0 => ResolvedModel::NotFound {
            requested: requested.to_string(),
            closest: closest_models(requested, namespaces),
        },
        1 => ResolvedModel::Bare {
            profile: matches[0].clone(),
            model: parsed.model,
        },
        _ => ResolvedModel::Ambiguous {
            candidates: matches,
            model: parsed.model,
        },
    }
}

/// A URL-safe, lowercase short name for an upstream's display name.
pub fn provider_alias(display_name: &str) -> String {
    let mut alias = String::with_capacity(display_name.len());
    let mut last_dash = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            alias.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !alias.is_empty() {
            alias.push('-');
            last_dash = true;
        }
    }
    while alias.ends_with('-') {
        alias.pop();
    }
    if alias.is_empty() {
        "provider".to_string()
    } else {
        alias
    }
}

/// One member of a route, resolved to something the gateway can call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMemberRef {
    pub profile: ModelProviderProfileId,
    pub model: String,
    /// Reasoning effort to ask for, overriding the Agent's request.
    pub reasoning_effort: Option<String>,
}

/// Chooses the member order, using observed usage for `LeastUsed`.
///
/// The strategy applies across members; the Key strategy inside the chosen
/// member is the existing `plan_route`, unchanged. A member with no usable Key
/// is still tried — its failure is what tells the caller why nothing answered,
/// and the caller may fail over to the next member.
///
/// `usage` is per Provider, in tokens spent in the current window. A member
/// with no recorded usage counts as zero, which is what makes a new Provider
/// get tried instead of waiting behind one that has already spent its budget.
/// Ties keep the configured order, so the strategy never shuffles a route that
/// has nothing to distinguish.
pub fn plan_members_with_usage(
    members: &[RouteMemberRef],
    strategy: vibex_core::RouteStrategy,
    rotation: u64,
    usage: &[(ModelProviderProfileId, u64)],
) -> Vec<RouteMemberRef> {
    let mut ordered = members.to_vec();
    match strategy {
        vibex_core::RouteStrategy::Ordered => {}
        vibex_core::RouteStrategy::RoundRobin => {
            if !ordered.is_empty() {
                let offset = (rotation as usize) % ordered.len();
                ordered.rotate_left(offset);
            }
        }
        vibex_core::RouteStrategy::LeastUsed => {
            ordered.sort_by_key(|member| {
                usage
                    .iter()
                    .find(|(profile, _)| profile == &member.profile)
                    .map(|(_, tokens)| *tokens)
                    .unwrap_or(0)
            });
        }
    }
    ordered
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedModel<'a> {
    /// The request named a route, which is resolved to its members.
    Route {
        route_id: String,
        requested: &'a str,
    },
    Qualified {
        profile: ModelProviderProfileId,
        model: &'a str,
    },
    Bare {
        profile: ModelProviderProfileId,
        model: &'a str,
    },
    Ambiguous {
        candidates: Vec<ModelProviderProfileId>,
        model: &'a str,
    },
    NotFound {
        requested: String,
        closest: Vec<String>,
    },
}

impl ResolvedModel<'_> {
    /// The model id the upstream should see.
    pub fn model(&self) -> &str {
        match self {
            Self::Route { requested, .. } => requested,
            Self::Qualified { model, .. }
            | Self::Bare { model, .. }
            | Self::Ambiguous { model, .. } => model,
            Self::NotFound { requested, .. } => requested,
        }
    }

    /// The Provider that should answer, when one is already decided.
    pub fn profile(&self) -> Option<&ModelProviderProfileId> {
        match self {
            Self::Qualified { profile, .. } | Self::Bare { profile, .. } => Some(profile),
            Self::Route { .. } | Self::Ambiguous { .. } | Self::NotFound { .. } => None,
        }
    }

    /// The route the request named, when it named one.
    pub fn route_id(&self) -> Option<&str> {
        match self {
            Self::Route { route_id, .. } => Some(route_id),
            _ => None,
        }
    }
}

fn closest_models(requested: &str, namespaces: &[ModelNamespace]) -> Vec<String> {
    let needle = GatewayModelRef::parse(requested).model.to_lowercase();
    let mut scored = Vec::new();
    for namespace in namespaces {
        for model in &namespace.models {
            let candidate = model.to_lowercase();
            let score = if candidate.contains(&needle) || needle.contains(&candidate) {
                2
            } else {
                let shared = common_prefix_len(&candidate, &needle);
                if shared >= 3 { 1 } else { 0 }
            };
            if score > 0 {
                scored.push((score, format!("{}/{}", namespace.alias, model)));
            }
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.into_iter().map(|(_, model)| model).take(3).collect()
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

/// Derives the key a conversation pins to.
///
/// An explicit session header wins because it is the only source that is
/// stable across the Agent's own retries. The body-derived fallback hashes the
/// system prompt, the tool set, and the first user message, which is what a
/// vendor's prompt cache keys on; it is deliberately scoped by Agent so two
/// Agents running the same task do not share a Key they should not.
pub fn conversation_key(
    agent_id: Option<&str>,
    headers: &[(String, String)],
    body: &serde_json::Value,
) -> Option<String> {
    // `prompt_cache_key` is Codex's own thread identity, so it is the most
    // precise source when present.
    if let Some(key) = body
        .get("prompt_cache_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty())
    {
        return Some(stable_conversation_key(agent_id, key));
    }
    // Only headers a client may plausibly set are read. Vibex's own ACP layer
    // already carries the session id, so the first entry is the one the runtime
    // controls; the rest are generic names an Agent CLI may send.
    for header in [
        "x-vibex-session",
        "x-session-id",
        "session_id",
        "conversation_id",
    ] {
        if let Some((_, value)) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header))
        {
            let value = value.trim();
            if !value.is_empty() && value.len() <= 256 {
                return Some(stable_conversation_key(agent_id, value));
            }
        }
    }
    let system = body
        .get("system")
        .or_else(|| body.get("instructions"))
        .map(|value| value.to_string())
        .unwrap_or_default();
    let tools = body
        .get("tools")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let first_user = first_user_message(body)?;
    let material = format!(
        "{}|{}|{}|{}",
        agent_id.unwrap_or_default(),
        system,
        tools,
        first_user
    );
    Some(stable_conversation_key(agent_id, &material))
}

fn stable_conversation_key(agent_id: Option<&str>, material: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent_id.unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(material.as_bytes());
    let digest = hasher.finalize();
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn first_user_message(body: &serde_json::Value) -> Option<String> {
    // Chat, Anthropic, and Gemini each name the message list differently, and
    // Responses uses `input`. All four are inspected so the fallback works
    // regardless of the inbound protocol.
    let items = body
        .get("messages")
        .or_else(|| body.get("contents"))
        .or_else(|| body.get("input"))
        .and_then(serde_json::Value::as_array)?;
    for item in items {
        let role = item
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("user");
        if role == "assistant" || role == "model" {
            continue;
        }
        let content = item
            .get("content")
            .or_else(|| item.get("parts"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let text = match content {
            serde_json::Value::String(text) => text,
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => continue,
        };
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(name: &str) -> ModelProviderProfileId {
        ModelProviderProfileId::parse(format!("model_provider_{name}")).unwrap()
    }

    fn candidates() -> Vec<CandidateKey> {
        vec![
            CandidateKey::new(profile("a"), RequestId::new(), 0).with_usage(100),
            CandidateKey::new(profile("a"), RequestId::new(), 1).with_usage(10),
            CandidateKey::new(profile("b"), RequestId::new(), 0).with_usage(50),
        ]
    }

    #[test]
    fn ordered_keeps_priority_and_puts_cooling_last() {
        let mut list = candidates();
        list[1] = list[1].clone().cooling(GatewayFailureKind::Rate);
        let plan = plan_route(RouteRequest {
            candidates: list,
            strategy: GatewayRoutingStrategy::Ordered,
            rotation: 0,
            pinned: None,
        });
        assert_eq!(plan.order[0].priority, 0);
        assert_eq!(plan.order[1].priority, 0);
        assert_eq!(plan.skipped_cooling, 1);
        assert_eq!(
            plan.order.last().unwrap().cooling,
            Some(GatewayFailureKind::Rate)
        );
        assert_eq!(plan.attemptable().count(), 2);
    }

    #[test]
    fn rotation_advances_one_position_per_request() {
        let list = candidates();
        let first = plan_route(RouteRequest {
            candidates: list.clone(),
            strategy: GatewayRoutingStrategy::RoundRobin,
            rotation: 0,
            pinned: None,
        });
        let second = plan_route(RouteRequest {
            candidates: list.clone(),
            strategy: GatewayRoutingStrategy::RoundRobin,
            rotation: 1,
            pinned: None,
        });
        let third = plan_route(RouteRequest {
            candidates: list,
            strategy: GatewayRoutingStrategy::RoundRobin,
            rotation: 2,
            pinned: None,
        });
        let ids = |plan: &RoutePlan| {
            plan.attemptable()
                .map(|candidate| candidate.credential.as_str().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first)[1..], ids(&second)[..2]);
        assert_eq!(ids(&second)[1..], ids(&third)[..2]);
    }

    #[test]
    fn least_used_picks_the_smallest_window() {
        let plan = plan_route(RouteRequest {
            candidates: candidates(),
            strategy: GatewayRoutingStrategy::LeastUsed,
            rotation: 0,
            pinned: None,
        });
        assert_eq!(plan.order[0].used_tokens, 10);
        assert_eq!(plan.order[2].used_tokens, 100);
    }

    /// The user's preferred Key leads every strategy, not only the ordered one.
    ///
    /// A preference is what the user asked for; rotation and usage decide the
    /// rest of the pool. Otherwise "Use first" would quietly mean "first only
    /// while this strategy is selected".
    #[test]
    fn a_preferred_key_leads_every_strategy() {
        let list = candidates();
        let preferred = list[1].credential.clone();
        let marked = |strategy, rotation| {
            let mut list = list.clone();
            list[1] = list[1].clone().preferred();
            plan_route(RouteRequest {
                candidates: list,
                strategy,
                rotation,
                pinned: None,
            })
        };

        for (strategy, rotation) in [
            (GatewayRoutingStrategy::Ordered, 0),
            (GatewayRoutingStrategy::RoundRobin, 1),
            (GatewayRoutingStrategy::RoundRobin, 2),
            (GatewayRoutingStrategy::LeastUsed, 0),
        ] {
            let plan = marked(strategy, rotation);
            assert_eq!(
                plan.order[0].credential, preferred,
                "{strategy:?} rotation {rotation} must still start with the preferred Key"
            );
        }

        // Only the preferred Key moves; the remaining pool keeps the size it
        // had, so failover still has every other Key to try.
        assert_eq!(
            marked(GatewayRoutingStrategy::RoundRobin, 1)
                .attemptable()
                .count(),
            3
        );
    }

    #[test]
    fn a_live_pin_wins_over_the_strategy() {
        let list = candidates();
        let pinned_key = (list[2].profile.clone(), list[2].credential.clone());
        let plan = plan_route(RouteRequest {
            candidates: list,
            strategy: GatewayRoutingStrategy::Ordered,
            rotation: 0,
            pinned: Some(pinned_key.clone()),
        });
        assert!(plan.pinned);
        assert_eq!(
            (&plan.order[0].profile, &plan.order[0].credential),
            (&pinned_key.0, &pinned_key.1)
        );
        assert_eq!(plan.attemptable().count(), 3);
    }

    #[test]
    fn a_cooling_pin_falls_back_and_is_not_reported_as_pinned() {
        let mut list = candidates();
        let profile = list[0].profile.clone();
        let credential = list[0].credential.clone();
        list[0] = list[0].clone().cooling(GatewayFailureKind::Quota);
        let plan = plan_route(RouteRequest {
            candidates: list,
            strategy: GatewayRoutingStrategy::Ordered,
            rotation: 0,
            pinned: Some((profile, credential)),
        });
        assert!(!plan.pinned);
        // The cooling pin moved to the tail instead of jumping the queue.
        assert_eq!(
            plan.order.last().map(|candidate| candidate.cooling),
            Some(Some(GatewayFailureKind::Quota))
        );
    }

    #[test]
    fn a_key_over_its_token_ceiling_is_skipped() {
        let mut list = candidates();
        list[0] = list[0].clone().with_token_limit(
            GatewayTokenLimit {
                window: vibex_core::GatewayTokenWindow::Day,
                tokens: 50,
            },
            50,
        );
        let over_ceiling = list[0].credential.clone();
        let plan = plan_route(RouteRequest {
            candidates: list,
            strategy: GatewayRoutingStrategy::Ordered,
            rotation: 0,
            pinned: None,
        });
        assert_eq!(plan.skipped_over_limit, 1);
        assert_eq!(plan.attemptable().count(), 2);
        // The over-ceiling Key is not attemptable even though it has the
        // highest priority.
        assert!(
            plan.attemptable()
                .all(|candidate| candidate.credential != over_ceiling)
        );
    }

    #[test]
    fn failover_is_possible_only_with_another_candidate() {
        let single = plan_route(RouteRequest {
            candidates: vec![CandidateKey::new(profile("a"), RequestId::new(), 0)],
            strategy: GatewayRoutingStrategy::Ordered,
            rotation: 0,
            pinned: None,
        });
        assert!(!single.may_fail_over());
    }

    #[test]
    fn failure_classification_separates_quota_from_rate() {
        assert_eq!(
            classify_failure(429, r#"{"error":{"code":"insufficient_quota"}}"#),
            GatewayFailureKind::Credit
        );
        assert_eq!(
            classify_failure(429, "Rate limit exceeded, retry in 20s"),
            GatewayFailureKind::Rate
        );
        assert_eq!(
            classify_failure(429, "Your monthly quota is exhausted"),
            GatewayFailureKind::Quota
        );
        assert_eq!(
            classify_failure(402, "insufficient balance"),
            GatewayFailureKind::Credit
        );
        assert_eq!(classify_failure(401, ""), GatewayFailureKind::Auth);
        assert_eq!(
            classify_failure(403, "invalid api key"),
            GatewayFailureKind::Auth
        );
        // A policy refusal is not a bad credential.
        assert_eq!(
            classify_failure(403, "region not supported"),
            GatewayFailureKind::Other
        );
        assert_eq!(classify_failure(503, ""), GatewayFailureKind::Upstream);
    }

    #[test]
    fn conversation_key_prefers_an_explicit_header() {
        let key = conversation_key(
            Some("codex"),
            &[("X-Session-Id".to_string(), "session-1".to_string())],
            &json!({"input": "hi"}),
        )
        .unwrap();
        assert_eq!(key, stable_conversation_key(Some("codex"), "session-1"));
        // The Agent scope changes the key, so two Agents on the same task do
        // not share a pin.
        assert_ne!(
            key,
            conversation_key(
                Some("claude"),
                &[("X-Session-Id".to_string(), "session-1".to_string())],
                &json!({"input": "hi"}),
            )
            .unwrap()
        );
    }

    #[test]
    fn conversation_key_falls_back_to_the_conversation_shape() {
        let body = json!({
            "system": "brief",
            "tools": [],
            "messages": [
                {"role": "user", "content": "read a file"},
                {"role": "assistant", "content": "ok"}
            ]
        });
        let first = conversation_key(Some("codex"), &[], &body).unwrap();
        let second = conversation_key(Some("codex"), &[], &body).unwrap();
        assert_eq!(first, second);
        let different = conversation_key(
            Some("codex"),
            &[],
            &json!({"messages": [{"role": "user", "content": "write a file"}]}),
        )
        .unwrap();
        assert_ne!(first, different);
    }

    #[test]
    fn prompt_cache_key_is_used_when_present() {
        let key = conversation_key(
            Some("codex"),
            &[],
            &json!({"prompt_cache_key": "thread-9", "input": "hi"}),
        )
        .unwrap();
        assert_eq!(key, stable_conversation_key(Some("codex"), "thread-9"));
    }

    #[test]
    fn model_resolution_accepts_a_full_id_an_alias_or_a_bare_id() {
        let alpha = profile("alpha");
        let beta = profile("beta");
        let namespaces = vec![
            ModelNamespace {
                profile: alpha.clone(),
                alias: "alpha".to_string(),
                models: vec!["deepseek-chat".to_string()],
            },
            ModelNamespace {
                profile: beta.clone(),
                alias: "beta".to_string(),
                models: vec!["deepseek-chat".to_string(), "kimi-k2".to_string()],
            },
        ];
        assert_eq!(
            resolve_model("beta/deepseek-chat", &namespaces),
            ResolvedModel::Qualified {
                profile: beta.clone(),
                model: "deepseek-chat"
            }
        );
        assert_eq!(
            resolve_model(&format!("{}/kimi-k2", beta.as_str()), &namespaces),
            ResolvedModel::Qualified {
                profile: beta.clone(),
                model: "kimi-k2"
            }
        );
        assert!(matches!(
            resolve_model("deepseek-chat", &namespaces),
            ResolvedModel::Ambiguous { .. }
        ));
        // A prefix that names no configured upstream is a miss, not a silent
        // fallthrough to some other Provider.
        assert!(matches!(
            resolve_model("ghost/deepseek-chat", &namespaces),
            ResolvedModel::NotFound { .. }
        ));
        let only_beta = vec![namespaces[1].clone()];
        assert!(matches!(
            resolve_model("kimi-k2", &only_beta),
            ResolvedModel::Bare { .. }
        ));
        match resolve_model("deepsek-chat", &namespaces) {
            ResolvedModel::NotFound { closest, .. } => {
                assert!(!closest.is_empty());
                assert!(closest[0].starts_with("alpha/") || closest[0].starts_with("beta/"));
            }
            other => panic!("expected a miss with suggestions, got {other:?}"),
        }
    }

    #[test]
    fn provider_alias_is_url_safe_and_stable() {
        assert_eq!(provider_alias("DeepSeek"), "deepseek");
        assert_eq!(provider_alias("Moonshot Kimi"), "moonshot-kimi");
        assert_eq!(provider_alias(" 智谱 GLM "), "glm");
        assert_eq!(provider_alias("!!!"), "provider");
    }
}

#[cfg(test)]
mod route_members {
    use super::*;

    fn member(profile: &str, model: &str) -> RouteMemberRef {
        RouteMemberRef {
            profile: ModelProviderProfileId::parse(profile).unwrap(),
            model: model.to_string(),
            reasoning_effort: None,
        }
    }

    #[test]
    fn ordered_keeps_the_route_order() {
        let members = vec![
            member("model_provider_a", "a"),
            member("model_provider_b", "b"),
        ];
        let ordered = plan_members_with_usage(&members, vibex_core::RouteStrategy::Ordered, 7, &[]);
        assert_eq!(
            ordered
                .iter()
                .map(|member| member.model.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn round_robin_advances_once_per_request_and_wraps() {
        let members = vec![
            member("model_provider_a", "a"),
            member("model_provider_b", "b"),
            member("model_provider_c", "c"),
        ];
        let first =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::RoundRobin, 0, &[]);
        assert_eq!(first[0].model, "a");
        let second =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::RoundRobin, 1, &[]);
        assert_eq!(second[0].model, "b");
        let wrapped =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::RoundRobin, 4, &[]);
        assert_eq!(wrapped[0].model, "b", "four mod three is one");
        // Every request still offers the whole set.
        assert_eq!(wrapped.len(), 3);
    }

    #[test]
    fn least_used_puts_the_least_spent_member_first() {
        let members = vec![
            member("model_provider_a", "a"),
            member("model_provider_b", "b"),
        ];
        // `b` has spent less, so it leads even though it is configured second.
        let usage = vec![
            (
                ModelProviderProfileId::parse("model_provider_a").unwrap(),
                9_000,
            ),
            (
                ModelProviderProfileId::parse("model_provider_b").unwrap(),
                12,
            ),
        ];
        let planned =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::LeastUsed, 0, &usage);
        assert_eq!(
            planned
                .iter()
                .map(|member| member.model.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn a_member_with_no_recorded_usage_counts_as_unspent() {
        let members = vec![
            member("model_provider_a", "a"),
            member("model_provider_new", "b"),
        ];
        // The known Provider has spent tokens; the new one has no record at all.
        // It must be tried first, not left behind a Provider already burning.
        let usage = vec![(
            ModelProviderProfileId::parse("model_provider_a").unwrap(),
            500,
        )];
        let planned =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::LeastUsed, 0, &usage);
        assert_eq!(planned[0].model, "b", "an unspent member goes first");
    }

    #[test]
    fn least_used_without_usage_keeps_the_configured_order() {
        let members = vec![
            member("model_provider_a", "a"),
            member("model_provider_b", "b"),
        ];
        let planned =
            plan_members_with_usage(&members, vibex_core::RouteStrategy::LeastUsed, 0, &[]);
        assert_eq!(
            planned
                .iter()
                .map(|member| member.model.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "with nothing to distinguish them, order is stable"
        );
    }

    #[test]
    fn an_empty_route_plans_to_nothing() {
        let planned = plan_members_with_usage(&[], vibex_core::RouteStrategy::RoundRobin, 5, &[]);
        assert!(planned.is_empty());
    }

    /// A refusal that names a shape problem is the protocol's fault, not the
    /// Key's.
    ///
    /// The distinction is load-bearing: treating these as Key failures is what
    /// cooled every Key three times and closed the gateway.
    #[test]
    fn a_shape_refusal_is_recognised_as_a_protocol_mismatch() {
        // The exact reply that started this: a Chat Completions endpoint handed
        // a Responses body.
        assert!(is_protocol_mismatch(
            422,
            r#"{"error":{"message":"messages[8]: unknown variant `function_call_output`, expected one of `text`, `image_url`, `file`"}}"#
        ));
        assert!(is_protocol_mismatch(415, "unsupported media type"));
        // A 400 only counts when the body says the shape is wrong.
        assert!(is_protocol_mismatch(
            400,
            r#"{"error":"failed to deserialize the JSON body into the target type"}"#
        ));
        assert!(
            !is_protocol_mismatch(400, r#"{"error":{"message":"content was rejected"}}"#),
            "a content refusal is the request's fault and keeps its treatment"
        );
        assert!(!is_protocol_mismatch(401, "invalid api key"));
        assert!(!is_protocol_mismatch(429, "rate limit exceeded"));
        assert!(
            !is_protocol_mismatch(500, "unknown variant"),
            "a server error is the vendor's problem whatever its wording"
        );
        // A 404 is decided by the caller, which knows whether another protocol
        // is left to try.
        assert!(!is_protocol_mismatch(404, "not found"));
    }
}
