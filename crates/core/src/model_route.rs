//! Reusable model routes.
//!
//! A route is the unit a session picks: the `ModelProviderProfile` and its
//! `AgentModelProviderBinding` say what a Provider *is* and which Agent reaches
//! it, while a route says *how* a request reaches a model — which sources are
//! tried, in what order, and under which policy.
//!
//! Two shapes exist:
//!
//! - [`RouteKind::ProviderGroup`] — the route composes whole Providers: every
//!   model their enabled catalogues share is offered once, and a request for
//!   that model is routed across the Providers that serve it. This is what
//!   makes a Provider set look like one Provider to a session. A group is the
//!   gateway's only input: the gateway owns no Provider list of its own.
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
///
/// It counts the routes a user composed — provider groups. A `Direct` route is
/// materialised from a Provider Profile by
/// `backfill_direct_routes`, one per direct Profile already on the installation,
/// so counting those would let a long Provider list exhaust the budget before a
/// single route was authored. See `ModelRouteRepository::count_user_authored`.
pub const MAX_ROUTES: usize = 32;

/// The prefix a provider group's model ids carry.
///
/// Every model the gateway serves is addressed through a group, so this is the
/// only prefix a request against the gateway may carry.
pub const PROVIDER_GROUP_MODEL_PREFIX: &str = "group/";

/// How a route delivers a model to an Agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// Served by the local gateway, composed from whole Providers.
    ///
    /// A provider group names no models of its own: its members are Providers,
    /// and the models it offers are the intersection of their enabled
    /// catalogues. One model id may therefore be served by several Providers,
    /// and the group's strategy picks which one answers. This is the only shape
    /// the gateway serves, and therefore its only source of upstreams.
    #[default]
    ProviderGroup,
    /// One Provider, called directly. The gateway is not involved.
    Direct,
}

impl RouteKind {
    pub const ALL: [Self; 2] = [Self::ProviderGroup, Self::Direct];

    pub const fn id(self) -> &'static str {
        match self {
            Self::ProviderGroup => "provider_group",
            Self::Direct => "direct",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == value.trim())
    }

    /// Whether the local gateway serves this kind.
    ///
    /// A direct route is the one shape that never reaches the listener.
    pub const fn via_gateway(self) -> bool {
        matches!(self, Self::ProviderGroup)
    }

    /// Direct routes carry exactly one member; gateway routes may carry more.
    pub const fn member_limit(self) -> usize {
        match self {
            Self::ProviderGroup => MAX_ROUTE_MEMBERS,
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

    /// One Provider as a member of a provider group.
    ///
    /// A group names Providers, not models: its model list is the intersection
    /// of their catalogues, so the member carries an empty model id and the
    /// provider is the whole address.
    pub fn provider(model_provider_profile_id: ModelProviderProfileId, order_index: i64) -> Self {
        Self::new(model_provider_profile_id, "", order_index)
    }

    /// Whether this member names a model rather than only a Provider.
    pub fn names_model(&self) -> bool {
        !self.model_id.trim().is_empty()
    }

    pub fn validate(&self) -> VibexResult<()> {
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

    /// The model checks that only apply to a member that names one.
    fn validate_model(&self) -> VibexResult<()> {
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
    /// Whether one conversation stays on one Key.
    ///
    /// Route-scoped rather than gateway-scoped: a group with several Keys per
    /// Provider may want to hold a conversation on one of them, while another
    /// group over the same Providers prefers to re-pick each request.
    ///
    /// Conversation affinity is the default because it is what preserves a
    /// vendor's prompt cache, and it is what the gateway did for every request
    /// before pinning became a property of the composition. A group that wants
    /// to spread load instead turns it off.
    #[serde(default)]
    pub pin: crate::GatewayPinScope,
    /// What happens when the member a conversation is pinned to fails.
    ///
    /// Route-scoped like `pin`, because the two are halves of one decision: a
    /// route that pins a conversation has to say how it lets go. The default is
    /// to switch and report it, which keeps a Provider outage from stranding a
    /// session; a route that mixes vendors can ask instead.
    #[serde(default)]
    pub failover: crate::GatewayFailover,
    /// A ceiling on the tokens this route may spend in its window.
    ///
    /// `None` means no route-level ceiling; per-Key limits still apply. A route
    /// that names several members needs its own ceiling because the per-Key ones
    /// do not compose: three Keys with a limit each still allow three times the
    /// spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<crate::GatewayTokenLimit>,
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
            kind: RouteKind::ProviderGroup,
            members: Vec::new(),
            strategy: RouteStrategy::Ordered,
            first_token_timeout_ms: None,
            context_tokens: None,
            pin: crate::GatewayPinScope::Conversation,
            failover: crate::GatewayFailover::Auto,
            token_limit: None,
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
    ///
    /// A provider group has no model-carrying members at all: its models are
    /// derived from the Providers' catalogues by
    /// [`intersect_provider_group_models`], so this reads empty for it.
    pub fn distinct_models(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for member in self.enabled_members().filter(|member| member.names_model()) {
            if !seen.contains(&member.model_id.as_str()) {
                seen.push(member.model_id.as_str());
            }
        }
        seen
    }

    /// Whether this route composes whole Providers.
    pub fn is_provider_group(&self) -> bool {
        self.kind == RouteKind::ProviderGroup
    }

    /// How many sources back this route.
    pub fn source_count(&self) -> usize {
        self.enabled_members().count()
    }

    /// The prefix every model of a provider group is addressed by.
    ///
    /// A group is several selectable models rather than one, so its ids carry
    /// the model after the route: `group/<id>/<model>`. The model has to live
    /// in the id because the group itself is not a model; the Provider is
    /// chosen per request, which is exactly what the id's first half leaves
    /// out.
    ///
    /// A direct route never reaches the gateway, so it has no gateway id and
    /// this prefix describes nothing callable; it exists for the group shape
    /// every gateway route now has.
    pub fn model_id(&self) -> String {
        format!("{}{}/", PROVIDER_GROUP_MODEL_PREFIX, self.id.as_str())
    }

    /// The id one model of a provider group is selected by.
    pub fn group_model_id(&self, model_id: &str) -> String {
        format!("{}{}", self.model_id(), model_id.trim().trim_matches('/'))
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
        if self.members.len() > MAX_ROUTE_MEMBERS {
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
            // A provider group names Providers, so a model on a member would be
            // a second, conflicting way to say which models it offers. A route
            // without groups names models, so the model is required there.
            if self.is_provider_group() {
                if member.names_model() {
                    return Err(VibexError::validation(
                        "route_group_member_model_unexpected",
                        "a provider group member names a Provider, not a model",
                    ));
                }
            } else {
                member.validate_model()?;
            }
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
        if self.is_provider_group() {
            let mut seen_providers: Vec<ModelProviderProfileId> = Vec::new();
            for member in &self.members {
                if seen_providers.contains(&member.model_provider_profile_id) {
                    return Err(VibexError::validation(
                        "route_group_provider_duplicate",
                        "a provider group lists the same Provider twice",
                    ));
                }
                seen_providers.push(member.model_provider_profile_id.clone());
            }
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

/// One model a provider group offers, with the Providers that serve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGroupModel {
    pub model_id: String,
    /// The Providers whose enabled catalogue carries the model, in the group's
    /// attempt order. A request for this model may be answered by any of them.
    pub provider_profile_ids: Vec<ModelProviderProfileId>,
    /// The capabilities shared by every Provider serving this model.
    pub capabilities: crate::ProviderModelCapabilities,
}

/// The models a provider group offers: the intersection of its Providers'
/// enabled catalogues.
///
/// Intersection rather than union is the whole contract. A session that picks a
/// model from the group is promised the same model whichever Provider answers,
/// so a model only one Provider serves cannot be offered: a strategy that
/// routes per request would eventually send it to a Provider that never heard
/// of it.
///
/// The model order follows the group's member order, then each catalogue's own
/// order, so the picker is stable across reads.
pub fn provider_group_models(
    route: &ModelRoute,
    providers: &[crate::ModelProviderProfile],
) -> Vec<ProviderGroupModel> {
    let members = route
        .enabled_members()
        .filter_map(|member| {
            providers
                .iter()
                .find(|profile| profile.id == member.model_provider_profile_id)
        })
        .collect::<Vec<_>>();
    let Some(first) = members.first() else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for model in first.configured_models.iter().filter(|model| model.enabled) {
        let mut serving = vec![first.id.clone()];
        let mut capabilities = vec![model.capabilities.clone()];
        let mut every_provider_serves = true;
        for provider in members.iter().skip(1) {
            let Some(entry) = provider
                .configured_models
                .iter()
                .find(|entry| entry.enabled && entry.id == model.id)
            else {
                every_provider_serves = false;
                break;
            };
            serving.push(provider.id.clone());
            capabilities.push(entry.capabilities.clone());
        }
        if !every_provider_serves {
            continue;
        }
        models.push(ProviderGroupModel {
            model_id: model.id.clone(),
            provider_profile_ids: serving,
            capabilities: intersect_route_capabilities(capabilities),
        });
    }
    models
}

/// A model id that names a provider group, split into the group and the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderGroupModelRef<'a> {
    pub group_id: &'a str,
    pub model_id: &'a str,
}

/// Splits a `group/<id>/<model>` model id.
///
/// Returns `None` for anything that is not group-shaped, so an ordinary
/// Provider model keeps resolving the way it did.
pub fn provider_group_model_ref(requested: &str) -> Option<ProviderGroupModelRef<'_>> {
    let rest = requested.strip_prefix(PROVIDER_GROUP_MODEL_PREFIX)?;
    let (group_id, model_id) = rest.split_once('/')?;
    let group_id = group_id.trim();
    let model_id = model_id.trim();
    (!group_id.is_empty() && !model_id.is_empty())
        .then_some(ProviderGroupModelRef { group_id, model_id })
}

/// Replaces a route, including its member order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteUpdateRequest {
    pub route: ModelRoute,
    pub expected_revision: i64,
}

/// Removes a route and every binding that pointed at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteDeleteRequest {
    pub model_route_id: ModelRouteId,
}

/// Lets one Agent use a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteBindRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
    /// Make this the route a new session starts on.
    #[serde(default)]
    pub make_default: bool,
}

/// Stops one Agent from using a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteUnbindRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
}

/// Chooses which of an Agent's routes a new session starts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteDefaultRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_member(order_index: i64) -> ModelRouteMember {
        ModelRouteMember::provider(ModelProviderProfileId::new(), order_index)
    }

    fn model_member(order_index: i64, model: &str) -> ModelRouteMember {
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
    fn a_direct_route_must_hold_exactly_one_member() {
        let one = route(RouteKind::Direct, vec![model_member(0, "a")]);
        assert!(one.validate().is_ok());

        let two = route(
            RouteKind::Direct,
            vec![model_member(0, "a"), model_member(1, "b")],
        );
        let error = two.validate().unwrap_err();
        assert_eq!(error.code, "route_direct_member_count_invalid");

        let none = route(RouteKind::Direct, vec![]);
        assert_eq!(none.validate().unwrap_err().code, "route_member_missing");
    }

    #[test]
    fn a_provider_group_member_limit_is_sixteen() {
        let at_limit = route(
            RouteKind::ProviderGroup,
            (0..16).map(provider_member).collect(),
        );
        assert!(at_limit.validate().is_ok());

        let over = route(
            RouteKind::ProviderGroup,
            (0..17).map(provider_member).collect(),
        );
        assert_eq!(
            over.validate().unwrap_err().code,
            "route_member_limit_exceeded"
        );
    }

    #[test]
    fn a_route_rejects_duplicate_positions() {
        let route = route(
            RouteKind::ProviderGroup,
            vec![provider_member(0), provider_member(0)],
        );
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_order_duplicate"
        );
    }

    #[test]
    fn a_disabled_member_keeps_its_place_but_is_not_counted() {
        let mut route = route(
            RouteKind::ProviderGroup,
            vec![provider_member(0), provider_member(1)],
        );
        route.members[1].enabled = false;
        assert!(route.validate().is_ok());
        assert_eq!(route.source_count(), 1);
        assert_eq!(route.members.len(), 2, "the place is kept");
    }

    #[test]
    fn every_gateway_model_id_is_a_group_id() {
        let route = route(RouteKind::ProviderGroup, vec![provider_member(0)]);
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
        assert_eq!(
            route.group_model_id("deepseek-chat"),
            format!("group/{}/deepseek-chat", route.id.as_str())
        );
    }

    #[test]
    fn a_direct_route_model_id_describes_no_gateway_entry() {
        // A direct route never reaches the gateway, so its id is only ever
        // compared against a gateway Profile's catalogue, never called.
        let route = route(RouteKind::Direct, vec![model_member(0, "deepseek-chat")]);
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
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

    fn group(providers: Vec<ModelProviderProfileId>) -> ModelRoute {
        ModelRoute {
            display_name: "Team".to_string(),
            kind: RouteKind::ProviderGroup,
            members: providers
                .into_iter()
                .enumerate()
                .map(|(index, provider)| ModelRouteMember::provider(provider, index as i64))
                .collect(),
            status: RouteStatus::Enabled,
            ..Default::default()
        }
    }

    fn provider(
        models: &[(&str, crate::ProviderModelCapabilities)],
    ) -> crate::ModelProviderProfile {
        crate::ModelProviderProfile {
            display_name: "DeepSeek".to_string(),
            status: crate::ModelProviderProfileStatus::Enabled,
            configured_models: models
                .iter()
                .map(|(id, capabilities)| crate::ModelProviderCatalogEntry {
                    id: (*id).to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: capabilities.clone(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_provider_group_offers_only_the_models_every_provider_serves() {
        let left = provider(&[
            ("shared", Default::default()),
            ("left-only", Default::default()),
        ]);
        let right = provider(&[
            ("shared", Default::default()),
            ("right-only", Default::default()),
        ]);
        let route = group(vec![left.id.clone(), right.id.clone()]);
        assert!(route.validate().is_ok());
        let models = provider_group_models(&route, &[left.clone(), right.clone()]);
        assert_eq!(models.len(), 1, "only the intersection survives");
        assert_eq!(models[0].model_id, "shared");
        assert_eq!(
            models[0].provider_profile_ids,
            vec![left.id.clone(), right.id.clone()]
        );
    }

    /// One Provider is a valid group: it is how a single upstream is put behind
    /// the gateway for its protocol translation.
    ///
    /// Nothing about the intersection breaks — the intersection of one set is
    /// that set — so the group simply offers the Provider's own catalogue.
    #[test]
    fn a_provider_group_may_hold_a_single_provider() {
        let only = provider(&[
            ("shared", Default::default()),
            ("another", Default::default()),
        ]);
        let route = group(vec![only.id.clone()]);
        assert!(route.validate().is_ok(), "one Provider must validate");
        let models = provider_group_models(&route, std::slice::from_ref(&only));
        assert_eq!(
            models
                .iter()
                .map(|model| model.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["shared", "another"],
            "a lone Provider offers its whole enabled catalogue"
        );
        assert_eq!(models[0].provider_profile_ids, vec![only.id.clone()]);
    }

    #[test]
    fn a_provider_group_claims_the_capabilities_all_serving_providers_share() {
        let left = provider(&[(
            "shared",
            crate::ProviderModelCapabilities {
                context_tokens: Some(128_000),
                image_input: Some(true),
                ..Default::default()
            },
        )]);
        let right = provider(&[(
            "shared",
            crate::ProviderModelCapabilities {
                context_tokens: Some(32_000),
                ..Default::default()
            },
        )]);
        let route = group(vec![left.id.clone(), right.id.clone()]);
        let models = provider_group_models(&route, &[left, right]);
        assert_eq!(models[0].capabilities.context_tokens, Some(32_000));
        assert_eq!(models[0].capabilities.image_input, None);
    }

    #[test]
    fn a_provider_group_member_names_a_provider_not_a_model() {
        let provider = ModelProviderProfileId::new();
        let mut route = group(vec![provider.clone()]);
        assert!(route.validate().is_ok());

        route.members[0].model_id = "deepseek-chat".to_string();
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_group_member_model_unexpected"
        );
    }

    #[test]
    fn a_provider_group_lists_a_provider_once() {
        let provider = ModelProviderProfileId::new();
        let route = group(vec![provider.clone(), provider]);
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_group_provider_duplicate"
        );
    }

    #[test]
    fn a_provider_group_model_id_names_the_group_and_the_model() {
        let route = group(vec![ModelProviderProfileId::new()]);
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
        assert_eq!(
            route.group_model_id("deepseek-chat"),
            format!("group/{}/deepseek-chat", route.id.as_str())
        );
        let id = route.group_model_id("deepseek-chat");
        let parsed = provider_group_model_ref(&id).expect("a group id parses");
        assert_eq!(parsed.group_id, route.id.as_str());
        assert_eq!(parsed.model_id, "deepseek-chat");
        assert!(provider_group_model_ref("route/standard").is_none());
    }
}
