//! Reusable model routes.
//!
//! A route is the unit a session picks: the `ModelProviderProfile` and its
//! `AgentModelProviderBinding` say what a Provider *is* and which Agent reaches
//! it, while a route says *how* a request reaches a model — which sources are
//! tried, in what order, and under which policy.
//!
//! Three shapes exist, and only two of them are records:
//!
//! - [`RouteKind::Gateway`] — the route is served by the local gateway, which
//!   may fail over across its members and translate protocols.
//! - [`RouteKind::Direct`] — the route names exactly one Provider and skips the
//!   gateway entirely. Its `base_url` is the Provider's, not the loopback one.
//! - An account (`RuntimeAuthSource::AgentAccount`) is *not* a route. Its
//!   destination is an `AgentAuthContext`, it is one per Agent, and it owns no
//!   members, so modelling it as a record would give `members` a meaning that
//!   changes by type. The route graph renders it as a node, not a row.
//!
//! Routes are global assets: they carry no `agent_id`. An Agent references one
//! through [`AgentModelRouteBinding`], the same way it references a shared
//! Provider through its binding. One route can therefore serve several Agents,
//! and editing its order reaches all of them at once.

use serde::{Deserialize, Serialize};

use crate::{
    AgentId, ModelProviderProfileId, ModelRouteId, ModelRouteMemberId, VibexError, VibexResult,
};

/// The most members one route may hold.
///
/// Sixteen is enough for "primary plus a failover per Provider" without turning
/// the route graph into a wall of curves.
pub const MAX_ROUTE_MEMBERS: usize = 16;

/// The most routes a user may keep.
///
/// This bounds the route list rather than the graph: a route is a saved
/// composition, and past this many the picker stops being a picker.
pub const MAX_ROUTES: usize = 32;

/// How a route delivers a model to an Agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// Served by the local gateway: members fail over, protocols translate.
    Gateway,
    /// One Provider, called directly. The gateway is not involved.
    Direct,
}

impl RouteKind {
    pub const ALL: [Self; 2] = [Self::Gateway, Self::Direct];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Direct => "direct",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == value.trim())
    }

    /// Direct routes carry exactly one member; gateway routes may carry more.
    pub const fn member_limit(self) -> usize {
        match self {
            Self::Gateway => MAX_ROUTE_MEMBERS,
            Self::Direct => 1,
        }
    }
}

/// How the members of a gateway route are tried.
///
/// These are the strategies the gateway can honour from data it already holds.
/// A quota-aware "smart" strategy would need vendor allowance windows, which an
/// API Key does not expose, so it is deliberately absent rather than faked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// The first member takes every request until it cannot answer.
    #[default]
    Ordered,
    /// Each request moves to the next member.
    RoundRobin,
    /// The member with the fewest tokens in the window goes first.
    LeastUsed,
}

impl RouteStrategy {
    pub const ALL: [Self; 3] = [Self::Ordered, Self::RoundRobin, Self::LeastUsed];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::RoundRobin => "round_robin",
            Self::LeastUsed => "least_used",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.id() == value.trim())
    }
}

/// One member: a model at one Provider, in the order it is tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteMember {
    pub id: ModelRouteMemberId,
    pub model_provider_profile_id: ModelProviderProfileId,
    /// The model id the Provider serves.
    pub model_id: String,
    /// Whether this member may serve a request.
    ///
    /// Disabling keeps the member's place in the order, so trying a route
    /// without one of its sources does not mean removing and re-adding it.
    pub enabled: bool,
    /// Position in the attempt order. The route's members are always stored
    /// sorted by this value.
    pub order_index: i64,
    /// Reasoning effort to ask for, overriding what the Agent requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ModelRouteMember {
    pub fn new(
        model_provider_profile_id: ModelProviderProfileId,
        model_id: impl Into<String>,
        order_index: i64,
    ) -> Self {
        Self {
            id: ModelRouteMemberId::new(),
            model_provider_profile_id,
            model_id: model_id.into(),
            enabled: true,
            order_index,
            reasoning_effort: None,
        }
    }

    pub fn validate(&self) -> VibexResult<()> {
        let model = self.model_id.trim();
        if model.is_empty() {
            return Err(VibexError::validation(
                "route_member_model_invalid",
                "a route member must name a model",
            ));
        }
        if model.chars().count() > 256 {
            return Err(VibexError::validation(
                "route_member_model_invalid",
                "a route member model id is longer than the supported length",
            ));
        }
        if self.order_index < 0 {
            return Err(VibexError::validation(
                "route_member_order_invalid",
                "a route member order must not be negative",
            ));
        }
        if let Some(effort) = self.reasoning_effort.as_deref() {
            let effort = effort.trim();
            if effort.is_empty() || effort.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_member_effort_invalid",
                    "a route member reasoning effort must be non-empty and bounded",
                ));
            }
        }
        Ok(())
    }
}

/// A reusable composition of model sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoute {
    pub id: ModelRouteId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: RouteKind,
    /// Members in attempt order.
    pub members: Vec<ModelRouteMember>,
    pub strategy: RouteStrategy,
    /// How long a member may take to produce its first token before the next
    /// one is asked instead.
    ///
    /// This exists because a vendor can accept a connection and then stall: the
    /// request never fails, so no error-based failover ever fires, and the turn
    /// hangs. `None` waits as long as it takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_timeout_ms: Option<u64>,
    /// An explicit context window, overriding the default "smallest member"
    /// rule. Only meaningful for a gateway route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    pub status: RouteStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Enabled,
    Disabled,
}

impl RouteStatus {
    pub const ALL: [Self; 2] = [Self::Enabled, Self::Disabled];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.id() == value.trim())
    }
}

impl Default for ModelRoute {
    /// An empty, disabled route.
    ///
    /// This exists so a caller — a fixture, or a page that has not loaded yet —
    /// can name only the fields it cares about. It is never a valid record to
    /// persist: `validate` rejects an empty name and an empty member list.
    fn default() -> Self {
        Self {
            id: ModelRouteId::new(),
            display_name: String::new(),
            description: None,
            kind: RouteKind::Gateway,
            members: Vec::new(),
            strategy: RouteStrategy::Ordered,
            first_token_timeout_ms: None,
            context_tokens: None,
            status: RouteStatus::Disabled,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

impl ModelRoute {
    pub const MAX_DISPLAY_NAME_LEN: usize = 160;

    /// The members that may actually serve a request, in attempt order.
    pub fn enabled_members(&self) -> impl Iterator<Item = &ModelRouteMember> {
        self.members.iter().filter(|member| member.enabled)
    }

    /// The models this route offers to an Agent, deduplicated but order-stable.
    ///
    /// A member is a `(Provider, model)` pair, so one model configured at three
    /// Providers appears three times in `members` and once here. This is what
    /// the model picker lists inside the route's group.
    pub fn distinct_models(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for member in self.enabled_members() {
            if !seen.contains(&member.model_id.as_str()) {
                seen.push(member.model_id.as_str());
            }
        }
        seen
    }

    /// How many sources back this route.
    pub fn source_count(&self) -> usize {
        self.enabled_members().count()
    }

    /// The model id an Agent asks the gateway for.
    ///
    /// One route is one selectable model: the Agent never sees the members.
    /// That is what keeps failover invisible to it — a member change must not
    /// require a new selection.
    pub fn model_id(&self) -> String {
        format!("route/{}", self.id.as_str())
    }

    /// Sorts members into attempt order. Callers mutate freely; `validate`
    /// requires the order to be dense and unique.
    pub fn sort_members(&mut self) {
        self.members.sort_by(|left, right| {
            left.order_index
                .cmp(&right.order_index)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
    }

    pub fn validate(&self) -> VibexResult<()> {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_DISPLAY_NAME_LEN {
            return Err(VibexError::validation(
                "route_display_name_invalid",
                "a route name must be non-empty and within the supported length",
            ));
        }
        if self.members.is_empty() {
            return Err(VibexError::validation(
                "route_member_missing",
                "a route must name at least one model",
            ));
        }
        if self.members.len() > RouteKind::Gateway.member_limit() {
            return Err(VibexError::validation(
                "route_member_limit_exceeded",
                "the route has more members than it supports",
            ));
        }
        if self.kind == RouteKind::Direct && self.members.len() != 1 {
            return Err(VibexError::validation(
                "route_direct_member_count_invalid",
                "a direct route must name exactly one model",
            ));
        }
        let mut seen_ids = Vec::new();
        let mut seen_orders = Vec::new();
        for member in &self.members {
            member.validate()?;
            if seen_ids.contains(&member.id.as_str()) {
                return Err(VibexError::validation(
                    "route_member_duplicate",
                    "a route contains the same member twice",
                ));
            }
            seen_ids.push(member.id.as_str());
            let order = usize::try_from(member.order_index).unwrap_or(usize::MAX);
            if seen_orders.contains(&order) {
                return Err(VibexError::validation(
                    "route_member_order_duplicate",
                    "two route members claim the same place in the order",
                ));
            }
            seen_orders.push(order);
        }
        if self.first_token_timeout_ms == Some(0) {
            return Err(VibexError::validation(
                "route_first_token_timeout_invalid",
                "a first-token timeout must be greater than zero",
            ));
        }
        if self.context_tokens == Some(0) {
            return Err(VibexError::validation(
                "route_context_invalid",
                "a route context window must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// One Agent's reference to a route.
///
/// Several routes may be bound to one Agent; exactly one is its default. The
/// default is what a new session starts on, so the invariant only holds among
/// the live bindings of one Agent, which the write path enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteBinding {
    pub id: crate::AgentModelRouteBindingId,
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
    pub is_default: bool,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_ms: Option<i64>,
}

impl AgentModelRouteBinding {
    pub fn new(agent_id: AgentId, model_route_id: ModelRouteId) -> Self {
        Self {
            id: crate::AgentModelRouteBindingId::new(),
            agent_id,
            model_route_id,
            is_default: false,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

/// The capabilities a route may claim, given its members.
///
/// Numeric maxima take the smallest member: if one member cannot hold a
/// prompt, the route cannot either. Booleans take the intersection: a route
/// only claims what every member supports, because a request may land on any of
/// them. A capability a member does not declare counts as unsupported —
/// absent is not "yes".
pub fn intersect_route_capabilities(
    members: impl IntoIterator<Item = crate::ProviderModelCapabilities>,
) -> crate::ProviderModelCapabilities {
    let caps = members.into_iter().collect::<Vec<_>>();
    if caps.is_empty() {
        return crate::ProviderModelCapabilities::default();
    }
    let smallest = |pick: fn(&crate::ProviderModelCapabilities) -> Option<u32>| {
        caps.iter().filter_map(pick).min()
    };
    let every = |pick: fn(&crate::ProviderModelCapabilities) -> Option<bool>| {
        caps.iter()
            .all(|caps| pick(caps) == Some(true))
            .then_some(true)
    };
    crate::ProviderModelCapabilities {
        reasoning: every(|caps| caps.reasoning),
        image_input: every(|caps| caps.image_input),
        pdf_input: every(|caps| caps.pdf_input),
        temperature: every(|caps| caps.temperature),
        context_tokens: smallest(|caps| caps.context_tokens),
        output_tokens: smallest(|caps| caps.output_tokens),
        reasoning_efforts: intersect_reasoning_efforts(&caps),
        ..Default::default()
    }
}

fn intersect_reasoning_efforts(
    caps: &[crate::ProviderModelCapabilities],
) -> Option<crate::ProviderModelReasoningEfforts> {
    let declared = caps
        .iter()
        .filter_map(|caps| caps.reasoning_efforts.clone())
        .collect::<Vec<_>>();
    if declared.len() != caps.len() {
        // A member that declares no effort table supports none of them, so the
        // route cannot promise any either.
        return None;
    }
    let first = declared.first()?;
    let mut levels = std::collections::BTreeMap::new();
    for (level, wire) in &first.0 {
        if declared.iter().all(|efforts| efforts.0.contains_key(level)) {
            levels.insert(*level, wire.clone());
        }
    }
    // A member whose table declares no levels offers none; the route then
    // offers none, rather than falling back to the levels of the others.
    if levels.is_empty() {
        return None;
    }
    Some(crate::ProviderModelReasoningEfforts(levels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(order_index: i64, model: &str) -> ModelRouteMember {
        ModelRouteMember::new(ModelProviderProfileId::new(), model, order_index)
    }

    fn route(kind: RouteKind, members: Vec<ModelRouteMember>) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            kind,
            members,
            status: RouteStatus::Enabled,
            ..Default::default()
        }
    }

    #[test]
    fn a_gateway_route_may_hold_several_members() {
        let route = route(
            RouteKind::Gateway,
            vec![member(0, "a"), member(1, "b"), member(2, "c")],
        );
        assert!(route.validate().is_ok());
        assert_eq!(route.source_count(), 3);
    }

    #[test]
    fn a_direct_route_must_hold_exactly_one_member() {
        let one = route(RouteKind::Direct, vec![member(0, "a")]);
        assert!(one.validate().is_ok());

        let two = route(RouteKind::Direct, vec![member(0, "a"), member(1, "b")]);
        let error = two.validate().unwrap_err();
        assert_eq!(error.code, "route_direct_member_count_invalid");

        let none = route(RouteKind::Direct, vec![]);
        assert_eq!(none.validate().unwrap_err().code, "route_member_missing");
    }

    #[test]
    fn the_gateway_member_limit_is_sixteen() {
        let at_limit = route(
            RouteKind::Gateway,
            (0..16).map(|index| member(index, "m")).collect(),
        );
        assert!(at_limit.validate().is_ok());

        let over = route(
            RouteKind::Gateway,
            (0..17).map(|index| member(index, "m")).collect(),
        );
        assert_eq!(
            over.validate().unwrap_err().code,
            "route_member_limit_exceeded"
        );
    }

    #[test]
    fn a_route_rejects_duplicate_positions() {
        let route = route(RouteKind::Gateway, vec![member(0, "a"), member(0, "b")]);
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_order_duplicate"
        );
    }

    #[test]
    fn a_disabled_member_keeps_its_place_but_is_not_counted() {
        let mut route = route(RouteKind::Gateway, vec![member(0, "a"), member(1, "b")]);
        route.members[1].enabled = false;
        assert!(route.validate().is_ok());
        assert_eq!(route.source_count(), 1);
        assert_eq!(route.members.len(), 2, "the place is kept");
    }

    #[test]
    fn one_route_is_one_model_id_however_many_members_it_has() {
        let route = route(
            RouteKind::Gateway,
            vec![member(0, "a"), member(1, "b"), member(2, "c")],
        );
        assert_eq!(route.model_id(), format!("route/{}", route.id.as_str()));
    }

    #[test]
    fn the_same_model_at_several_providers_is_one_distinct_model() {
        let route = route(
            RouteKind::Gateway,
            vec![
                member(0, "deepseek-v4-flash"),
                member(1, "deepseek-v4-flash"),
                member(2, "kimi-k3"),
            ],
        );
        assert_eq!(route.source_count(), 3);
        assert_eq!(
            route.distinct_models(),
            vec!["deepseek-v4-flash", "kimi-k3"]
        );
    }

    #[test]
    fn route_capabilities_take_the_smallest_window_and_the_shared_features() {
        let narrow = crate::ProviderModelCapabilities {
            context_tokens: Some(32_000),
            output_tokens: Some(4_000),
            reasoning: Some(true),
            image_input: Some(false),
            ..Default::default()
        };
        let wide = crate::ProviderModelCapabilities {
            context_tokens: Some(200_000),
            output_tokens: Some(8_000),
            reasoning: Some(true),
            image_input: Some(true),
            ..Default::default()
        };
        let merged = intersect_route_capabilities([narrow, wide]);
        assert_eq!(
            merged.context_tokens,
            Some(32_000),
            "the smallest window wins"
        );
        assert_eq!(merged.output_tokens, Some(4_000));
        assert_eq!(merged.reasoning, Some(true), "both reason");
        assert_eq!(merged.image_input, None, "one cannot take an image");
    }

    #[test]
    fn an_undeclared_capability_is_not_claimed() {
        let declared = crate::ProviderModelCapabilities {
            reasoning: Some(true),
            ..Default::default()
        };
        let silent = crate::ProviderModelCapabilities::default();
        let merged = intersect_route_capabilities([declared, silent]);
        assert_eq!(merged.reasoning, None, "absent is not yes");
    }
}
