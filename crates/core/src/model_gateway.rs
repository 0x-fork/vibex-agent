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

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelProviderProfileId, RequestId};
use crate::provider_projection::ModelProviderProfile;
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

/// Concurrency and rate limits for one upstream.
///
/// Vendors count requests per account, so these are per Key rather than global.
/// A Key that is already busy is not a failure: the gateway queues for it, and
/// only answers `429` when the queue itself is full or its wait ran out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountLimits {
    /// Requests allowed in flight at once, across every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// Requests allowed to start in any one minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rpm: Option<u32>,
    /// How many requests may wait for a slot. `None` means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_limit: Option<u32>,
    /// How long a request may wait for a slot before it is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<u64>,
}

impl GatewayAccountLimits {
    /// The longest a request may wait before the gateway answers `429`.
    pub const DEFAULT_QUEUE_WAIT_MS: u64 = 30_000;

    pub fn is_configured(&self) -> bool {
        self.max_concurrency.is_some() || self.max_rpm.is_some()
    }

    pub fn validate(&self) -> VibexResult<()> {
        if self.max_concurrency == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_concurrency_invalid",
                "a concurrency limit must be at least one",
            ));
        }
        if self.max_rpm == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_rpm_invalid",
                "an RPM limit must be at least one",
            ));
        }
        if self.queue_limit == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_queue_limit_invalid",
                "a queue limit must be at least one",
            ));
        }
        if self.queue_wait_ms == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_queue_wait_invalid",
                "a queue wait must be at least one millisecond",
            ));
        }
        Ok(())
    }

    pub fn queue_wait(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.queue_wait_ms.unwrap_or(Self::DEFAULT_QUEUE_WAIT_MS))
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
    /// Concurrency and rate limits, keyed by credential id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub account_limits: BTreeMap<RequestId, GatewayAccountLimits>,
    /// The Key the router tries first for each Provider, when the user asked
    /// for one. A Provider without an entry keeps its configured Key order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preferred_keys: BTreeMap<ModelProviderProfileId, RequestId>,
}

impl Default for GatewayPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            routing: GatewayRoutingStrategy::Ordered,
            pin: GatewayPinScope::Conversation,
            token_limit: None,
            translate: GatewayTranslatePolicy::default(),
            account_limits: BTreeMap::new(),
            preferred_keys: BTreeMap::new(),
        }
    }
}

impl GatewayPolicy {
    pub fn validate(&self) -> VibexResult<()> {
        if let Some(limit) = self.token_limit.as_ref() {
            limit.validate()?;
        }
        for limits in self.account_limits.values() {
            limits.validate()?;
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
    /// The listener is up but upstream calls have failed repeatedly, so the
    /// gateway has stepped out of the way and Agents fall back to their direct
    /// Provider configuration.
    ///
    /// This is distinct from `Unavailable`: the port is still bound, and the
    /// state clears itself once an upstream answers again. Reporting it is the
    /// whole point — a silent fallback would leave the user believing
    /// automatic routing is still in effect.
    Degraded,
    /// The user turned automatic routing off. The surface stays visible.
    Disabled,
}

impl ModelGatewayStatus {
    pub const fn is_routable(self) -> bool {
        matches!(self, Self::Running)
    }

    /// The listener is bound even though routing may be bypassed.
    pub const fn is_listening(self) -> bool {
        matches!(self, Self::Running | Self::Degraded)
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
    ///
    /// A rejected Key is marked invalid, but the request may still succeed on
    /// a different Key: that is the whole point of a pool. Only a reply that
    /// has already reached the client ends the attempt loop.
    pub const fn should_fail_over(self) -> bool {
        true
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
    /// What the router did before a Key answered. Absent on a row written
    /// before the trace existed, and on a request that never got as far as a
    /// routing decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GatewayRouteTrace>,
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

/// The payload version of [`GatewayRouteTrace`] as it is stored on a row.
///
/// The trace is the one ledger column whose shape may grow, so it carries its
/// own version. A reader that meets a version it does not know reports the row
/// as having no trace rather than failing the whole list: the ledger is a
/// diagnostic, and one unreadable annotation must not hide every other request.
pub const GATEWAY_ROUTE_TRACE_SCHEMA_VERSION: u32 = 1;

/// One Key the gateway tried and set aside before another one answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteAttempt {
    pub credential_id: RequestId,
    /// Why this Key did not serve the reply.
    pub failure: GatewayFailureKind,
}

/// What the router did with one request, as the ledger records it.
///
/// Only ids appear here. The trace explains a failover — which Key was cooled
/// or over its ceiling, and which Key was tried and failed — without ever
/// naming Key material, a prompt, or a reply. Payload schema
/// [`GATEWAY_ROUTE_TRACE_SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteTrace {
    pub schema_version: u32,
    /// Keys set aside before the first attempt because they were cooling.
    #[serde(default)]
    pub skipped_cooling: u32,
    /// Keys set aside because they had spent their local token ceiling.
    #[serde(default)]
    pub skipped_over_limit: u32,
    /// Keys tried, in order, that did not answer.
    #[serde(default)]
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members this request walked past before one answered.
    ///
    /// A route may fail over across Providers as well as across Keys, and the
    /// two mean different things: a Key cooled down, while a whole member was
    /// unavailable. Keeping them in one list would make "which Provider did it
    /// try" unreadable.
    #[serde(default)]
    pub skipped_members: Vec<GatewaySkippedMember>,
}

/// One route member that did not answer, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySkippedMember {
    pub model_provider_profile_id: ModelProviderProfileId,
    /// The protocol the member would have been called with.
    #[serde(default)]
    pub wire_protocol_id: String,
    pub failure: GatewayFailureKind,
}

impl GatewayRouteTrace {
    /// A fresh trace at the current payload version.
    pub fn new(skipped_cooling: usize, skipped_over_limit: usize) -> Self {
        Self {
            schema_version: GATEWAY_ROUTE_TRACE_SCHEMA_VERSION,
            skipped_cooling: u32::try_from(skipped_cooling).unwrap_or(u32::MAX),
            skipped_over_limit: u32::try_from(skipped_over_limit).unwrap_or(u32::MAX),
            failed_attempts: Vec::new(),
            skipped_members: Vec::new(),
        }
    }

    /// Whether the trace says anything beyond "the first Key answered".
    pub fn is_unremarkable(&self) -> bool {
        self.skipped_cooling == 0
            && self.skipped_over_limit == 0
            && self.failed_attempts.is_empty()
            && self.skipped_members.is_empty()
    }

    /// Whether a reader can interpret this payload.
    pub fn is_current(&self) -> bool {
        self.schema_version == GATEWAY_ROUTE_TRACE_SCHEMA_VERSION
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
    /// The user wants the Agent to walk through the gateway.
    ///
    /// This survives a stopped listener. The Agent is only actually projected
    /// onto the gateway while [`Self::active`] is true.
    pub enabled: bool,
    /// The Agent is currently pointed at the gateway.
    ///
    /// When the listener is down the hidden Profile is removed so the Agent
    /// falls back to a direct Provider; the user's choice stays in `enabled`.
    #[serde(default)]
    pub active: bool,
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
    /// Present when this entry is a route rather than one upstream's model.
    ///
    /// The Config Center shows the two differently: a route names a saved
    /// composition and has no single Provider or protocol, while an upstream
    /// entry belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    /// How many members can answer a route.
    #[serde(default)]
    pub source_count: usize,
    /// How many distinct model names a route offers.
    #[serde(default)]
    pub model_count: usize,
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
    /// This Key's concurrency and rate limits, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<GatewayAccountLimits>,
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
    /// The Provider that answered, by id.
    ///
    /// The display name beside it is for reading; two Providers may share one,
    /// and a surface that filters the ledger by Provider must not conflate
    /// them.
    #[serde(default)]
    pub model_provider_profile_id: ModelProviderProfileId,
    pub provider_name: String,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_served: Option<String>,
    pub wire_in: String,
    pub wire_out: String,
    pub status: Option<u16>,
    pub total_tokens: u64,
    /// The split behind `total_tokens`, so a detail view can name each part.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    pub outcome: GatewayRequestOutcome,
    /// The Key that answered, under the name its Provider gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
    /// The route, once a Key has answered and the trace says something the
    /// caller could not already see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GatewayRouteSummary>,
    pub created_at_ms: i64,
}

impl GatewayRequestSummary {
    /// Projects one stored row for a client.
    ///
    /// Key ids become the names their Provider gave them, because a client
    /// showing a route is explaining a decision, not exposing an id table. A
    /// Key that has since been removed keeps its row and loses only its name.
    pub fn from_log(mut log: ModelGatewayRequestLog, profiles: &[ModelProviderProfile]) -> Self {
        let profile = profiles
            .iter()
            .find(|profile| profile.id == log.model_provider_profile_id);
        let name_of = |credential: &RequestId| {
            profile.and_then(|profile| {
                profile
                    .credentials
                    .iter()
                    .find(|candidate| &candidate.id == credential)
                    .map(|candidate| candidate.display_name.clone())
            })
        };
        let route = log
            .route
            .take()
            .filter(|trace| !trace.is_unremarkable())
            .map(|trace| GatewayRouteSummary {
                skipped_cooling: trace.skipped_cooling,
                skipped_over_limit: trace.skipped_over_limit,
                failed_attempts: trace
                    .failed_attempts
                    .into_iter()
                    .map(|attempt| GatewayRouteAttemptSummary {
                        credential_name: name_of(&attempt.credential_id),
                        credential_id: attempt.credential_id,
                        failure: attempt.failure,
                    })
                    .collect(),
                skipped_members: trace
                    .skipped_members
                    .into_iter()
                    .map(|member| GatewaySkippedMemberSummary {
                        provider_name: profiles
                            .iter()
                            .find(|candidate| candidate.id == member.model_provider_profile_id)
                            .map(|candidate| candidate.display_name.clone()),
                        model_provider_profile_id: member.model_provider_profile_id,
                        wire_protocol_id: member.wire_protocol_id,
                        failure: member.failure,
                    })
                    .collect(),
            });
        Self {
            model_provider_profile_id: profile
                .map(|profile| profile.id.clone())
                .unwrap_or_else(|| log.model_provider_profile_id.clone()),
            total_tokens: log.total_tokens(),
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            cached_read_tokens: log.cached_read_tokens,
            request_id: log.request_id,
            agent_id: log.agent_id,
            provider_name: profile
                .map(|profile| profile.display_name.clone())
                .unwrap_or_else(|| log.model_provider_profile_id.as_str().to_string()),
            model_requested: log.model_requested,
            model_served: log.model_served,
            wire_in: log.wire_in,
            wire_out: log.wire_out,
            status: log.status,
            latency_ms: log.latency_ms,
            outcome: log.outcome,
            credential_name: log.credential_id.as_ref().and_then(&name_of),
            route,
            created_at_ms: log.created_at_ms,
        }
    }
}

/// One request the gateway is serving right now.
///
/// This is memory state, never a ledger row: a request that is still running
/// has no outcome to record, and the graph needs to draw it *while* it runs.
/// The identifiers are the ones a client already sees; no prompt, reply, or Key
/// material appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInflight {
    pub request_id: RequestId,
    /// The Agent that sent it, when the request carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The route, when the request named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route_id: Option<String>,
    /// The model id the Agent asked for.
    pub requested_model: String,
    /// The member currently answering, and its place in the attempt order.
    pub model_provider_profile_id: ModelProviderProfileId,
    /// Which member of the route this is, counting from one: the graph numbers
    /// attempts so a failover reads as "the second source".
    pub attempt: u32,
    /// The Key serving this attempt, when one has been chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<RequestId>,
    /// The protocol the member is called with.
    pub wire_protocol_id: String,
    pub started_at_ms: i64,
    /// Set once the first token has arrived. A request with a deadline and no
    /// first token yet is the one that may be abandoned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at_ms: Option<i64>,
}

impl GatewayInflight {
    /// Whether this request has produced any of its reply yet.
    pub fn has_first_token(&self) -> bool {
        self.first_token_at_ms.is_some()
    }
}

/// What the gateway tells a client about a request it is serving.
///
/// These are the transitions a graph needs, not every internal step: a request
/// begins, it starts answering, or it ends. Member-level retries are folded
/// into a new `Began` with a higher attempt number, because that is what the
/// graph draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum GatewayEvent {
    /// A request started, or moved to another member.
    Began(GatewayInflight),
    /// The member produced its first token.
    FirstToken { request_id: RequestId, at_ms: i64 },
    /// The request finished, however it ended.
    Ended {
        request_id: RequestId,
        outcome: GatewayRequestOutcome,
    },
}

impl GatewayEvent {
    /// The request this event is about.
    pub fn request_id(&self) -> &RequestId {
        match self {
            Self::Began(inflight) => &inflight.request_id,
            Self::FirstToken { request_id, .. } => request_id,
            Self::Ended { request_id, .. } => request_id,
        }
    }
}

/// The route, with each Key named the way its Provider labels it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteSummary {
    pub skipped_cooling: u32,
    pub skipped_over_limit: u32,
    pub failed_attempts: Vec<GatewayRouteAttemptSummary>,
    /// Members the request walked past, with their Providers named.
    #[serde(default)]
    pub skipped_members: Vec<GatewaySkippedMemberSummary>,
}

/// One skipped route member, named for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySkippedMemberSummary {
    /// Absent once the Provider has been removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    pub model_provider_profile_id: ModelProviderProfileId,
    pub wire_protocol_id: String,
    pub failure: GatewayFailureKind,
}

/// One failed attempt, named for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteAttemptSummary {
    /// Absent once the Key has been removed from its Provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
    pub credential_id: RequestId,
    pub failure: GatewayFailureKind,
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
    /// Concurrency and rate limits per credential. An omitted entry leaves that
    /// Key unlimited, and the whole map replaces the stored one.
    #[serde(default)]
    pub account_limits: BTreeMap<RequestId, GatewayAccountLimits>,
    /// Which protocol directions the gateway may translate.
    ///
    /// The write replaces the whole policy, like `account_limits`: an older
    /// client that omits the field sends the default, where every direction is
    /// allowed.
    #[serde(default)]
    pub translate: GatewayTranslatePolicy,
    /// Routing-state changes submitted with this write.
    ///
    /// These are commands, not a snapshot: only the Keys named here are
    /// touched, so turning one Key off does not rewrite every other Key's
    /// cooldown or limits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_updates: Vec<GatewayAccountUpdate>,
    pub expected_revision: i64,
}

/// One Key's routing-state change, as the Config Center submits it.
///
/// `enabled` and `preferred` are independent: a Key can be turned off without
/// changing which Key is preferred, and a Key can be preferred while it is
/// off (the preference takes effect again once it is enabled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountUpdate {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    /// `Some(true)` re-enables the Key and clears any failure cooldown, so an
    /// account repaired outside Vibex can be tried again; `Some(false)` takes
    /// it out of rotation; `None` leaves it as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// `Some(true)` makes this the Key the router tries first for its
    /// Provider; `Some(false)` returns that Provider to its configured order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<bool>,
}

impl GatewayAccountUpdate {
    pub fn validate(&self) -> VibexResult<()> {
        if self.enabled.is_none() && self.preferred.is_none() {
            return Err(VibexError::validation(
                "model_gateway_account_update_empty",
                "an account update must change `enabled` or `preferred`",
            ));
        }
        Ok(())
    }
}

/// The editor's question: what does the catalogue suggest for this model?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitySuggestionRequest {
    /// The catalogue's provider id, such as `deepseek`.
    pub provider_id: String,
    pub model_id: String,
}

/// One suggested field, with how the match was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCapabilitySuggestion {
    /// `contextTokens`, `outputTokens`, `reasoning`, `imageInput`, `pdfInput`,
    /// or `temperature`.
    pub fields: Vec<GatewayCapabilityField>,
    /// The catalogue matched a longer id this one extends.
    pub approximate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCapabilityField {
    pub field: String,
    /// The value as the editor's own field spelling, so it can be shown without
    /// a second mapping table.
    pub value: String,
}

impl GatewayCapabilitySuggestion {
    /// One suggested field's raw value.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|entry| entry.field == name)
            .map(|entry| entry.value.as_str())
    }

    /// A suggested numeric field.
    pub fn number(&self, name: &str) -> Option<u32> {
        self.field(name).and_then(|value| value.parse().ok())
    }

    /// A suggested boolean field.
    pub fn flag(&self, name: &str) -> Option<bool> {
        self.field(name).map(|value| value == "true")
    }

    /// Writes one suggested field into a declaration and records it as having
    /// come from the catalogue.
    ///
    /// Adoption is a deliberate act on one field, so it is per field rather
    /// than "apply everything". A field the suggestion does not carry is left
    /// exactly as it was, and the return value says whether anything changed.
    pub fn adopt(&self, capabilities: &mut crate::ProviderModelCapabilities, field: &str) -> bool {
        let adopted = match field {
            "contextTokens" => self
                .number(field)
                .map(|value| capabilities.context_tokens = Some(value)),
            "outputTokens" => self
                .number(field)
                .map(|value| capabilities.output_tokens = Some(value)),
            "reasoning" => self
                .flag(field)
                .map(|value| capabilities.reasoning = Some(value)),
            "imageInput" => self
                .flag(field)
                .map(|value| capabilities.image_input = Some(value)),
            "pdfInput" => self
                .flag(field)
                .map(|value| capabilities.pdf_input = Some(value)),
            "temperature" => self
                .flag(field)
                .map(|value| capabilities.temperature = Some(value)),
            _ => None,
        };
        if adopted.is_some() {
            capabilities.set_source(field, crate::ModelCapabilitySource::Catalog);
            true
        } else {
            false
        }
    }
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
            route: None,
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

    /// An empty command is a client bug, not a no-op: it would bump the
    /// settings revision and rewrite settings for no visible reason.
    #[test]
    fn an_account_update_must_change_something() {
        let profile = ModelProviderProfileId::new();
        let update = GatewayAccountUpdate {
            model_provider_profile_id: profile.clone(),
            credential_id: RequestId::new(),
            enabled: None,
            preferred: None,
        };
        assert_eq!(
            update.validate().unwrap_err().code,
            "model_gateway_account_update_empty"
        );

        for (enabled, preferred) in [
            (Some(false), None),
            (None, Some(true)),
            (Some(true), Some(false)),
        ] {
            let update = GatewayAccountUpdate {
                model_provider_profile_id: profile.clone(),
                credential_id: RequestId::new(),
                enabled,
                preferred,
            };
            update.validate().unwrap();
        }
    }

    /// Preference is settings, so it round-trips with the policy and stays out
    /// of the Key's own state (which only holds health).
    #[test]
    fn a_preferred_key_round_trips_in_the_policy() {
        let profile = ModelProviderProfileId::new();
        let credential = RequestId::new();
        let policy = GatewayPolicy {
            preferred_keys: BTreeMap::from([(profile.clone(), credential.clone())]),
            ..GatewayPolicy::default()
        };
        let encoded = serde_json::to_string(&policy).unwrap();
        assert!(encoded.contains("preferredKeys"), "{encoded}");
        let decoded: GatewayPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.preferred_keys.get(&profile), Some(&credential));

        // An older payload without the field still loads: preference is
        // additive, not a migration gate.
        let bare: GatewayPolicy =
            serde_json::from_str(r#"{"enabled":true,"routing":"ordered","pin":"conversation"}"#)
                .unwrap();
        assert!(bare.preferred_keys.is_empty());
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

    /// A summary names the Keys a route used, and says nothing when there is
    /// nothing to explain.
    ///
    /// The two behaviours are one contract: the UI shows a route only when the
    /// trace has something a reader could not already see, so a first-try
    /// success must not produce one.
    #[test]
    fn a_request_summary_names_the_keys_its_route_used() {
        let profile_id = ModelProviderProfileId::new();
        let primary = RequestId::new();
        let backup = RequestId::new();
        let credential = |id: RequestId, name: &str| crate::ModelProviderCredentialReference {
            id,
            display_name: name.to_string(),
            status: crate::AgentCredentialStatus::Ready,
            credential: crate::AgentCredential::Local { runtime: None },
            revision: 1,
        };
        let profile = ModelProviderProfile {
            id: profile_id.clone(),
            display_name: "DeepSeek".to_string(),
            credentials: vec![
                credential(primary.clone(), "Primary"),
                credential(backup.clone(), "Backup"),
            ],
            ..ModelProviderProfile::default()
        };
        let log = |route: Option<GatewayRouteTrace>| ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: None,
            model_provider_profile_id: profile_id.clone(),
            credential_id: Some(backup.clone()),
            model_requested: "deepseek/deepseek-chat".to_string(),
            model_served: None,
            wire_in: "openai_chat_completions".to_string(),
            wire_out: "openai_chat_completions".to_string(),
            status: Some(200),
            input_tokens: 0,
            output_tokens: 0,
            cached_read_tokens: 0,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: Some(0),
            outcome: GatewayRequestOutcome::Ok,
            route,
            created_at_ms: 0,
        };

        let unremarkable = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace::new(0, 0))),
            std::slice::from_ref(&profile),
        );
        assert_eq!(unremarkable.provider_name, "DeepSeek");
        assert_eq!(unremarkable.credential_name.as_deref(), Some("Backup"));
        assert!(unremarkable.route.is_none());

        let failover = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace {
                failed_attempts: vec![GatewayRouteAttempt {
                    credential_id: primary.clone(),
                    failure: GatewayFailureKind::Quota,
                }],
                ..GatewayRouteTrace::new(1, 0)
            })),
            std::slice::from_ref(&profile),
        );
        let route = failover.route.expect("a failover is worth reporting");
        assert_eq!(route.skipped_cooling, 1);
        assert_eq!(route.failed_attempts.len(), 1);
        assert_eq!(
            route.failed_attempts[0].credential_name.as_deref(),
            Some("Primary")
        );
        assert_eq!(route.failed_attempts[0].failure, GatewayFailureKind::Quota);

        // A Key removed since the request keeps its id and loses only its name,
        // and a row whose Provider is gone keeps its own provider id.
        let gone = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace {
                failed_attempts: vec![GatewayRouteAttempt {
                    credential_id: RequestId::new(),
                    failure: GatewayFailureKind::Rate,
                }],
                ..GatewayRouteTrace::new(0, 0)
            })),
            &[],
        );
        assert!(gone.provider_name.starts_with("model_provider_"));
        assert!(gone.credential_name.is_none());
        assert!(
            gone.route
                .expect("the route survives a removed Key")
                .failed_attempts[0]
                .credential_name
                .is_none()
        );
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
        // An auth failure invalidates this Key, but the next one may work.
        assert!(GatewayFailureKind::Auth.should_fail_over());
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
