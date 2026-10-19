//! Local model gateway contracts.
//!
//! The gateway is one loopback HTTP endpoint that Agents can be pointed at
//! instead of a vendor address. It owns request-level routing — which upstream
//! answers, which of its Keys is used, what happens when that Key is cooling
//! down — while the Provider Profile and Agent binding contracts keep owning
//! the durable configuration.
//!
//! Two rules shape everything in this module:
//!
//! - Routing state is request state. Nothing here may enter an Agent process
//!   fingerprint, so switching a Key never restarts an Agent process.
//! - Usage is counted in tokens only. Vendor billing cannot be read through an
//!   API key, so the contract deliberately carries no price, cost, or currency
//!   field. A local token ceiling is a user-set ceiling on observed tokens,
//!   not a claim about a vendor's quota.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelProviderProfileId, RequestId};
use crate::{VibexError, VibexResult, unix_timestamp_ms};

/// How the gateway picks one of a Provider's Keys for the next request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRoutingStrategy {
    /// Use the first ready Key until it cannot answer, then the next one.
    #[default]
    Ordered,
    /// Advance one Key per request so usage is spread across the pool.
    RoundRobin,
    /// Prefer the Key that has carried the fewest tokens in this window.
    LeastUsed,
}

impl GatewayRoutingStrategy {
    pub const ALL: [Self; 3] = [Self::Ordered, Self::RoundRobin, Self::LeastUsed];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::RoundRobin => "round_robin",
            Self::LeastUsed => "least_used",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ordered => "In order",
            Self::RoundRobin => "Rotate",
            Self::LeastUsed => "Least used",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|strategy| strategy.id() == value.trim())
    }
}

/// How long a conversation stays on the Key it started on.
///
/// Vendors cache a prompt per account, so re-picking a Key on every turn
/// throws that cache away. Pinning trades a little load balance for a
/// materially cheaper turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPinScope {
    /// Re-pick per request.
    Off,
    /// Keep a conversation on one Key until it is invalidated.
    #[default]
    Conversation,
}

impl GatewayPinScope {
    pub const ALL: [Self; 2] = [Self::Off, Self::Conversation];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Conversation => "conversation",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.id() == value.trim())
    }
}

/// The window a local token ceiling is measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayTokenWindow {
    Day,
    Week,
    Month,
}

impl GatewayTokenWindow {
    pub const ALL: [Self; 3] = [Self::Day, Self::Week, Self::Month];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// The length of one window in milliseconds.
    pub const fn duration_ms(self) -> i64 {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        match self {
            Self::Day => DAY_MS,
            Self::Week => 7 * DAY_MS,
            Self::Month => 30 * DAY_MS,
        }
    }
}

/// A local ceiling on tokens the gateway has observed for one Provider.
///
/// This is a soft limit by construction: the gateway only sees requests it
/// served itself, and reaches the ceiling after a request has already been
/// spent. What it guarantees is that once the ceiling is crossed the Provider
/// stops being selected until the window rolls over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTokenLimit {
    pub window: GatewayTokenWindow,
    pub tokens: u64,
}

impl GatewayTokenLimit {
    pub fn validate(&self) -> VibexResult<()> {
        if self.tokens == 0 {
            return Err(VibexError::validation(
                "model_gateway_token_limit_invalid",
                "a token limit must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Which protocol translations the gateway may perform.
///
/// A translation the user disables is refused with a 400 instead of being
/// performed silently: a request that cannot be expressed in the target
/// protocol should fail loudly rather than arrive mangled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTranslatePolicy {
    pub openai_chat_completions: bool,
    pub openai_responses: bool,
    pub anthropic_messages: bool,
    pub google_generative_ai: bool,
}

impl Default for GatewayTranslatePolicy {
    fn default() -> Self {
        Self {
            openai_chat_completions: true,
            openai_responses: true,
            anthropic_messages: true,
            google_generative_ai: true,
        }
    }
}

impl GatewayTranslatePolicy {
    pub fn allows(&self, wire_protocol_id: &str) -> bool {
        match wire_protocol_id {
            crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => self.openai_chat_completions,
            crate::WIRE_PROTOCOL_OPENAI_RESPONSES => self.openai_responses,
            crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => self.anthropic_messages,
            crate::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => self.google_generative_ai,
            // Protocols the gateway cannot translate are never gated here;
            // passthrough is the only route for them.
            _ => true,
        }
    }
}

/// Durable routing policy for the local gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayPolicy {
    pub enabled: bool,
    pub routing: GatewayRoutingStrategy,
    pub pin: GatewayPinScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<GatewayTokenLimit>,
    #[serde(default)]
    pub translate: GatewayTranslatePolicy,
}

impl Default for GatewayPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            routing: GatewayRoutingStrategy::Ordered,
            pin: GatewayPinScope::Conversation,
            token_limit: None,
            translate: GatewayTranslatePolicy::default(),
        }
    }
}

impl GatewayPolicy {
    pub fn validate(&self) -> VibexResult<()> {
        if let Some(limit) = self.token_limit.as_ref() {
            limit.validate()?;
        }
        Ok(())
    }
}

/// The gateway's whole durable configuration.
///
/// Upstreams are ordinary shared Provider Profiles; the gateway only keeps
/// their ids and the order they are tried in. That is what lets one Provider
/// serve several Agents without duplicating its endpoints or Keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewaySettings {
    pub display_name: String,
    /// The persisted listening port, or `0` before the first successful bind.
    pub port: u16,
    pub upstreams: Vec<ModelProviderProfileId>,
    pub policy: GatewayPolicy,
    pub revision: i64,
    pub updated_at_ms: i64,
}

impl Default for ModelGatewaySettings {
    fn default() -> Self {
        Self {
            display_name: "Vibex Gateway".to_string(),
            port: 0,
            upstreams: Vec::new(),
            policy: GatewayPolicy::default(),
            revision: 1,
            updated_at_ms: 0,
        }
    }
}

impl ModelGatewaySettings {
    /// The largest display name the Config Center accepts, matching the
    /// Provider Profile limit so the gateway row is not the odd one out.
    pub const MAX_DISPLAY_NAME_LEN: usize = 160;
    pub const MAX_UPSTREAMS: usize = 64;

    pub fn validate(&self) -> VibexResult<()> {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_DISPLAY_NAME_LEN {
            return Err(VibexError::validation(
                "model_gateway_display_name_invalid",
                "gateway display name must be non-empty and within the supported length",
            ));
        }
        if self.upstreams.len() > Self::MAX_UPSTREAMS {
            return Err(VibexError::validation(
                "model_gateway_upstream_limit_exceeded",
                "the gateway has more upstream Providers than it supports",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for upstream in &self.upstreams {
            if !seen.insert(upstream.as_str()) {
                return Err(VibexError::validation(
                    "model_gateway_upstream_duplicate",
                    "the gateway lists the same upstream Provider twice",
                ));
            }
        }
        // A port below 1024 is not usable by an unprivileged loopback service,
        // and a port of 0 means "not assigned yet" rather than "random".
        if (1..1024).contains(&self.port) {
            return Err(VibexError::validation(
                "model_gateway_port_invalid",
                "the gateway port must be above 1023",
            ));
        }
        self.policy.validate()
    }
}

/// Lifecycle state of the listening service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelGatewayStatus {
    /// The runtime has not started the service yet.
    Starting,
    Running,
    /// The service could not bind or was shut down. Agents fall back to their
    /// direct Provider configuration and the UI says so.
    Unavailable,
    /// The user turned automatic routing off. The surface stays visible.
    Disabled,
}

impl ModelGatewayStatus {
    pub const fn is_routable(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// A redacted, UI-facing snapshot of the running service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewayRuntimeState {
    pub status: ModelGatewayStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    pub updated_at_ms: i64,
}

impl Default for ModelGatewayRuntimeState {
    fn default() -> Self {
        Self {
            status: ModelGatewayStatus::Starting,
            port: None,
            upstream_count: None,
            notice: None,
            updated_at_ms: unix_timestamp_ms(),
        }
    }
}

impl ModelGatewayRuntimeState {
    pub fn running(port: u16, upstream_count: usize) -> Self {
        Self {
            status: ModelGatewayStatus::Running,
            port: Some(port),
            upstream_count: Some(upstream_count),
            notice: None,
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    pub fn unavailable(notice: impl Into<String>) -> Self {
        Self {
            status: ModelGatewayStatus::Unavailable,
            port: None,
            upstream_count: None,
            notice: Some(notice.into()),
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    /// The address an Agent's base URL should point at, when the service runs.
    pub fn base_url(&self) -> Option<String> {
        self.port
            .filter(|_| self.status == ModelGatewayStatus::Running)
            .map(|port| format!("http://127.0.0.1:{port}"))
    }
}

/// Why a Key is not currently selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayFailureKind {
    /// The account is out of money.
    Credit,
    /// The plan's window is used up.
    Quota,
    /// Requests are arriving faster than allowed.
    Rate,
    /// The credential itself is rejected.
    Auth,
    /// The proxy in front of the request failed.
    Proxy,
    /// The upstream could not be reached or answered with an unusable error.
    Upstream,
    /// Something the gateway could not classify.
    Other,
}

impl GatewayFailureKind {
    /// How long a Key sits out after this kind of failure, in milliseconds.
    ///
    /// `auth` is absent on purpose: a rejected credential does not heal with
    /// time, so the Key becomes invalid instead of cooling down.
    pub const fn cooldown_ms(self) -> Option<i64> {
        match self {
            Self::Credit => Some(30 * 60 * 1000),
            Self::Quota => Some(15 * 60 * 1000),
            Self::Rate => Some(30 * 1000),
            Self::Auth => None,
            Self::Proxy => Some(60 * 1000),
            Self::Upstream | Self::Other => Some(10 * 1000),
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Quota => "quota",
            Self::Rate => "rate",
            Self::Auth => "auth",
            Self::Proxy => "proxy",
            Self::Upstream => "upstream",
            Self::Other => "other",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        [
            Self::Credit,
            Self::Quota,
            Self::Rate,
            Self::Auth,
            Self::Proxy,
            Self::Upstream,
            Self::Other,
        ]
        .into_iter()
        .find(|kind| kind.id() == value.trim())
    }

    /// Whether another Key should be tried for the same request.
    pub const fn should_fail_over(self) -> bool {
        !matches!(self, Self::Auth)
    }
}

/// Durable per-Key routing state. Key material never appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderAccountState {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub enabled: bool,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<GatewayFailureKind>,
    pub consecutive_failures: i64,
    pub updated_at_ms: i64,
}

impl ModelProviderAccountState {
    pub fn ready(
        model_provider_profile_id: ModelProviderProfileId,
        credential_id: RequestId,
    ) -> Self {
        Self {
            model_provider_profile_id,
            credential_id,
            enabled: true,
            priority: 0,
            cooldown_until_ms: None,
            cooldown_reason: None,
            consecutive_failures: 0,
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    /// Whether this Key may be selected at `now_ms`.
    ///
    /// An authentication failure has no expiry: the credential is wrong until
    /// the user replaces it, so it never becomes ready again on its own.
    pub fn is_ready_at(&self, now_ms: i64) -> bool {
        self.enabled
            && self.cooldown_reason != Some(GatewayFailureKind::Auth)
            && self.cooldown_until_ms.is_none_or(|until| until <= now_ms)
    }

    /// The visible state of one Key, in the order the UI needs to explain it.
    pub fn visibility_at(&self, now_ms: i64) -> GatewayKeyState {
        if !self.enabled {
            return GatewayKeyState::Disabled;
        }
        match self.cooldown_reason {
            Some(GatewayFailureKind::Auth) => GatewayKeyState::Invalid,
            Some(kind) if self.cooldown_until_ms.is_some_and(|until| until > now_ms) => {
                GatewayKeyState::Cooling {
                    kind,
                    until_ms: self.cooldown_until_ms,
                }
            }
            _ => GatewayKeyState::Ready,
        }
    }
}

/// What the Config Center shows for one Key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayKeyState {
    Ready,
    Cooling {
        kind: GatewayFailureKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_ms: Option<i64>,
    },
    /// Authentication was rejected; the user has to replace the Key.
    Invalid,
    /// The user turned this Key off.
    Disabled,
}

impl GatewayKeyState {
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Cooling { .. } => "cooling",
            Self::Invalid => "invalid",
            Self::Disabled => "disabled",
        }
    }
}

/// One row of the gateway's request ledger.
///
/// This record deliberately holds no prompt, no reply, no image bytes, and no
/// Key material: only the shape of the decision and the token counts the
/// upstream reported. It is pruned after
/// [`ModelGatewayRequestLog::RETENTION_MS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewayRequestLog {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub model_provider_profile_id: ModelProviderProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<RequestId>,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_served: Option<String>,
    pub wire_in: String,
    pub wire_out: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    #[serde(default)]
    pub cached_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    pub outcome: GatewayRequestOutcome,
    pub created_at_ms: i64,
}

impl ModelGatewayRequestLog {
    /// The ledger keeps a rolling week of metadata.
    pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_read_tokens)
            .saturating_add(self.cached_write_tokens)
            .saturating_add(self.reasoning_tokens)
    }
}

/// How a gateway request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRequestOutcome {
    Ok,
    /// A Key failed before the reply began and another one answered.
    Failover,
    /// The provider's Keys were all cooling down.
    Cooldown,
    /// The request was refused before reaching an upstream.
    Rejected,
    Failed,
}

impl GatewayRequestOutcome {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failover => "failover",
            Self::Cooldown => "cooldown",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        [
            Self::Ok,
            Self::Failover,
            Self::Cooldown,
            Self::Rejected,
            Self::Failed,
        ]
        .into_iter()
        .find(|outcome| outcome.id() == value.trim())
    }
}

/// Tokens observed for one Provider in one local ceiling window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTokenWindowUsage {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub window: GatewayTokenWindow,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub used_tokens: u64,
}

impl GatewayTokenWindowUsage {
    pub const fn is_open_at(&self, now_ms: i64) -> bool {
        self.window_start_ms <= now_ms && now_ms < self.window_end_ms
    }

    pub const fn remaining(&self, limit_tokens: u64) -> u64 {
        limit_tokens.saturating_sub(self.used_tokens)
    }
}

/// One conversation's pinned Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConversationPin {
    pub conversation_key: String,
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub chosen_at_ms: i64,
    pub expires_at_ms: i64,
}

impl GatewayConversationPin {
    /// A conversation with no traffic for this long is re-picked, because the
    /// vendor's prompt cache would have expired anyway.
    pub const TTL_MS: i64 = 30 * 60 * 1000;

    pub fn is_live_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms > now_ms
    }
}

/// One Key as the gateway sees it, for the routing decision and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeyCandidate {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub priority: i64,
    pub state: GatewayKeyState,
    pub used_tokens: u64,
}

impl GatewayKeyCandidate {
    pub const fn is_selectable(&self) -> bool {
        self.state.is_ready()
    }
}

/// Which upstream answered a request, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRouteDecision {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub pinned: bool,
    pub skipped_cooling: usize,
    pub failover_attempts: usize,
}

/// The model a request names, split into its optional Provider prefix and id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayModelRef<'a> {
    pub provider: Option<&'a str>,
    pub model: &'a str,
}

impl<'a> GatewayModelRef<'a> {
    /// Splits `provider/model` without treating a model id that merely
    /// contains a slash as qualified.
    pub fn parse(value: &'a str) -> Self {
        match value.split_once('/') {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => Self {
                provider: Some(provider),
                model,
            },
            _ => Self {
                provider: None,
                model: value,
            },
        }
    }
}

/// One Agent's relationship to the gateway, as the Config Center shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAgentBinding {
    pub agent_id: String,
    pub display_name: String,
    /// The Agent walks through the gateway.
    pub enabled: bool,
    /// The Agent manages its own provider and cannot be routed.
    pub unsupported: bool,
    /// The protocol the Agent speaks to the gateway, for the model list.
    pub wire_protocol_id: String,
}

/// A Provider the user could add as a gateway upstream.
///
/// The gateway's upstream list uses the shared v2 Provider identity, while the
/// Config Center still lists legacy Profiles. The authority resolves the two
/// once, so a client never has to derive one id from the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUpstreamCandidate {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub display_name: String,
    pub model_count: usize,
    pub key_count: usize,
    /// The Provider is already an upstream.
    pub selected: bool,
}

/// One model the gateway serves, for the Config Center's read-only list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayModelSummary {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub provider_name: String,
    pub model_id: String,
    /// The id an Agent's model picker shows.
    pub display_id: String,
    pub wire_protocol_id: String,
}

/// The whole Config Center read for the gateway.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySnapshot {
    pub settings: ModelGatewaySettings,
    pub runtime: ModelGatewayRuntimeState,
    pub upstreams: Vec<GatewayUpstreamSummary>,
    pub models: Vec<GatewayModelSummary>,
    pub agents: Vec<GatewayAgentBinding>,
    /// Every Provider that could become an upstream.
    #[serde(default)]
    pub upstream_candidates: Vec<GatewayUpstreamCandidate>,
}

impl GatewaySnapshot {
    /// The number of Keys currently selectable across every upstream.
    pub fn ready_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| key.state.is_ready())
            .count()
    }

    /// The number of Keys sitting out a cooldown.
    pub fn cooling_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| matches!(key.state, GatewayKeyState::Cooling { .. }))
            .count()
    }

    /// The number of Keys the vendor rejected outright.
    pub fn invalid_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| matches!(key.state, GatewayKeyState::Invalid))
            .count()
    }

    /// Whether the surface can actually serve a request.
    pub fn is_routable(&self) -> bool {
        self.runtime.status.is_routable()
            && !self.upstreams.is_empty()
            && self.ready_key_count() > 0
    }
}

/// One upstream as the Config Center shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUpstreamSummary {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub display_name: String,
    pub model_count: usize,
    pub keys: Vec<GatewayKeySummary>,
}

/// One Key's visible state. The secret itself is never part of this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayKeySummary {
    pub credential_id: RequestId,
    pub display_name: String,
    pub state: GatewayKeyState,
    #[serde(default)]
    pub used_tokens: u64,
}

/// The Config Center's request to point one Agent at the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAgentUpdateRequest {
    pub agent_id: String,
    pub enabled: bool,
}

/// One request the gateway served, for the diagnostics list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequestSummary {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub provider_name: String,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_served: Option<String>,
    pub wire_in: String,
    pub wire_out: String,
    pub status: Option<u16>,
    pub total_tokens: u64,
    pub outcome: GatewayRequestOutcome,
    pub created_at_ms: i64,
}

/// The settings mutation the Config Center submits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySettingsUpdateRequest {
    pub display_name: String,
    pub routing: GatewayRoutingStrategy,
    pub pin: GatewayPinScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<GatewayTokenLimit>,
    pub upstreams: Vec<ModelProviderProfileId>,
    pub expected_revision: i64,
}

/// Where one Provider Profile field lands in a gateway-aware editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFieldVisibility {
    Editable,
    ReadOnly,
    Hidden,
}

impl ProfileFieldVisibility {
    pub const fn is_editable(self) -> bool {
        matches!(self, Self::Editable)
    }

    pub const fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}

/// The visibility the Config Center applies to a Provider Profile's fields.
///
/// A regular Provider is edited exactly as it is. The built-in gateway appears
/// in the same list but only exposes what a user can meaningfully change: its
/// address, credentials, and aggregated model catalogue are computed by the
/// runtime, so editing them would only break every binding that points at it.
pub fn profile_field_visibility(is_gateway: bool, field: &str) -> ProfileFieldVisibility {
    if !is_gateway {
        return ProfileFieldVisibility::Editable;
    }
    match field {
        "displayName" | "status" | "routing" | "pin" | "tokenLimit" | "upstreams" => {
            ProfileFieldVisibility::Editable
        }
        "usedByAgents" | "runtimeStatus" | "protocolCapabilities" | "keyState" => {
            ProfileFieldVisibility::ReadOnly
        }
        _ => ProfileFieldVisibility::Hidden,
    }
}

/// The verdict for a batch of Keys after one failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayKeyHealth {
    Ready,
    Cooling {
        kind: GatewayFailureKind,
        until_ms: i64,
    },
    Invalid,
}

impl GatewayKeyHealth {
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

impl fmt::Display for GatewayRoutingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_contracts_carry_no_money_fields() {
        let settings = ModelGatewaySettings {
            upstreams: vec![
                ModelProviderProfileId::parse(format!(
                    "model_provider_{}",
                    RequestId::new().as_str()
                ))
                .unwrap(),
            ],
            policy: GatewayPolicy {
                token_limit: Some(GatewayTokenLimit {
                    window: GatewayTokenWindow::Month,
                    tokens: 20_000_000,
                }),
                ..GatewayPolicy::default()
            },
            ..ModelGatewaySettings::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap().to_lowercase();
        for forbidden in [
            "cost", "price", "currency", "amount", "usd", "budget", "micros",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "gateway settings must not carry `{forbidden}`: {encoded}"
            );
        }

        let log = ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: Some("codex".to_string()),
            model_provider_profile_id: ModelProviderProfileId::new(),
            credential_id: Some(RequestId::new()),
            model_requested: "deepseek/deepseek-chat".to_string(),
            model_served: Some("deepseek-chat".to_string()),
            wire_in: crate::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            wire_out: crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            status: Some(200),
            input_tokens: 1024,
            output_tokens: 180,
            cached_read_tokens: 800,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: Some(42),
            outcome: GatewayRequestOutcome::Ok,
            created_at_ms: unix_timestamp_ms(),
        };
        let encoded = serde_json::to_string(&log).unwrap().to_lowercase();
        for forbidden in [
            "cost", "price", "currency", "amount", "usd", "budget", "micros",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "gateway request log must not carry `{forbidden}`: {encoded}"
            );
        }
    }

    #[test]
    fn account_state_separates_cooling_from_invalid() {
        let profile = ModelProviderProfileId::new();
        let credential = RequestId::new();
        let now = 1_000;

        let mut state = ModelProviderAccountState::ready(profile.clone(), credential.clone());
        assert_eq!(state.visibility_at(now), GatewayKeyState::Ready);
        assert!(state.is_ready_at(now));

        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(now + 30_000);
        assert_eq!(
            state.visibility_at(now),
            GatewayKeyState::Cooling {
                kind: GatewayFailureKind::Rate,
                until_ms: Some(now + 30_000)
            }
        );
        assert!(!state.is_ready_at(now));
        assert!(state.is_ready_at(now + 30_000));

        // An auth failure has no duration: the Key is invalid until replaced.
        state.cooldown_reason = Some(GatewayFailureKind::Auth);
        state.cooldown_until_ms = None;
        assert_eq!(state.visibility_at(now), GatewayKeyState::Invalid);
        assert!(!state.is_ready_at(now));

        state.cooldown_reason = None;
        state.enabled = false;
        assert_eq!(state.visibility_at(now), GatewayKeyState::Disabled);
    }

    #[test]
    fn routing_strategies_round_trip_by_id() {
        for strategy in GatewayRoutingStrategy::ALL {
            assert_eq!(
                GatewayRoutingStrategy::from_id(strategy.id()),
                Some(strategy)
            );
        }
        assert_eq!(GatewayRoutingStrategy::from_id("smart"), None);
        assert_eq!(GatewayRoutingStrategy::from_id("pace"), None);
    }

    #[test]
    fn settings_reject_duplicate_upstreams_and_privileged_ports() {
        let profile = ModelProviderProfileId::new();
        let mut settings = ModelGatewaySettings {
            upstreams: vec![profile.clone(), profile.clone()],
            ..ModelGatewaySettings::default()
        };
        assert_eq!(
            settings.validate().unwrap_err().code,
            "model_gateway_upstream_duplicate"
        );

        settings.upstreams = vec![profile];
        settings.port = 80;
        assert_eq!(
            settings.validate().unwrap_err().code,
            "model_gateway_port_invalid"
        );

        settings.port = 3425;
        settings.validate().unwrap();
    }

    #[test]
    fn token_limit_requires_a_positive_ceiling() {
        let limit = GatewayTokenLimit {
            window: GatewayTokenWindow::Day,
            tokens: 0,
        };
        assert_eq!(
            limit.validate().unwrap_err().code,
            "model_gateway_token_limit_invalid"
        );
        GatewayTokenLimit {
            window: GatewayTokenWindow::Day,
            tokens: 1,
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn gateway_profile_hides_computed_fields() {
        assert_eq!(
            profile_field_visibility(true, "endpoints"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "credentials"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "configuredModels"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "routing"),
            ProfileFieldVisibility::Editable
        );
        assert_eq!(
            profile_field_visibility(false, "endpoints"),
            ProfileFieldVisibility::Editable
        );
    }

    #[test]
    fn model_ref_only_splits_a_real_provider_prefix() {
        assert_eq!(
            GatewayModelRef::parse("deepseek/deepseek-chat"),
            GatewayModelRef {
                provider: Some("deepseek"),
                model: "deepseek-chat"
            }
        );
        assert_eq!(
            GatewayModelRef::parse("deepseek-chat"),
            GatewayModelRef {
                provider: None,
                model: "deepseek-chat"
            }
        );
        assert_eq!(
            GatewayModelRef::parse("open-r1/"),
            GatewayModelRef {
                provider: None,
                model: "open-r1/"
            }
        );
    }

    #[test]
    fn failure_kinds_classify_cooldowns() {
        assert_eq!(
            GatewayFailureKind::Credit.cooldown_ms(),
            Some(30 * 60 * 1000)
        );
        assert_eq!(GatewayFailureKind::Auth.cooldown_ms(), None);
        assert!(!GatewayFailureKind::Auth.should_fail_over());
        assert!(GatewayFailureKind::Rate.should_fail_over());
        assert_eq!(
            GatewayKeyHealth::from_failure(GatewayFailureKind::Auth, 5),
            GatewayKeyHealth::Invalid
        );
        assert_eq!(
            GatewayKeyHealth::from_failure(GatewayFailureKind::Proxy, 5),
            GatewayKeyHealth::Cooling {
                kind: GatewayFailureKind::Proxy,
                until_ms: 5 + 60_000
            }
        );
    }
}
