//! The gateway service: configuration, listener lifecycle, and one request.
//!
//! The service is owned by the desktop runtime. It reads durable configuration
//! from the same database the Config Center writes, keeps only request-level
//! state in memory, and binds one loopback port. Binding failure never fails
//! runtime startup: the status becomes [`ModelGatewayStatus::Unavailable`], the
//! UI says so, and Agents keep their direct Provider configuration.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use vibex_config_switch::{ProviderConfigService, secrets};
use vibex_core::{
    GatewayConversationPin, GatewayFailureKind, GatewayPinScope, GatewayPolicy,
    GatewayRequestOutcome, GatewayRouteAttempt, GatewayRouteTrace, GatewayTokenWindow,
    GatewayTokenWindowUsage, ModelGatewayRequestLog, ModelGatewayRuntimeState,
    ModelGatewaySettings, ModelGatewayStatus, ModelProviderAccountState,
    ModelProviderCredentialReference, ModelProviderEndpoint, ModelProviderEndpointKind,
    ModelProviderProfileId, ModelProviderProfileStatus, RequestId,
};
use vibex_db::{
    DbConnection, GatewayConversationPinRepository, GatewayTokenWindowRepository,
    ModelGatewayRequestLogRepository, ModelGatewaySettingsRepository,
    ModelProviderAccountStateRepository, apply_migrations, open_database,
};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalUsage, ReasoningRequest};
use crate::route::{
    CandidateKey, KeyHealth, ResolvedModel, RouteRequest, classify_failure, conversation_key,
    plan_route, resolve_model,
};
use crate::stream::{ReplyAccumulator, SseBuffer, UsageSniffer};
use crate::wire::parse_reply;
use crate::wire::{
    StreamEncoder, WireProtocol, build_upstream_request, decoder, encoder, parse_request,
    render_response, sse_frame,
};

/// A resolved upstream: one shared Provider Profile the gateway may call.
#[derive(Debug, Clone)]
pub struct GatewayUpstream {
    pub profile_id: ModelProviderProfileId,
    pub display_name: String,
    pub endpoints: Vec<ModelProviderEndpoint>,
    pub models: Vec<String>,
    pub credentials: Vec<ModelProviderCredentialReference>,
    pub keys: Vec<GatewayKeySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeySummary {
    pub credential_id: RequestId,
    pub display_name: String,
    pub state: vibex_core::GatewayKeyState,
    pub used_tokens: u64,
}

impl GatewayUpstream {
    /// The endpoint this protocol should call, if the upstream publishes one.
    pub fn endpoint_for(&self, wire_protocol_id: &str) -> Option<&ModelProviderEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| {
                endpoint.kind == ModelProviderEndpointKind::Api
                    && endpoint.wire_protocol_id.as_deref() == Some(wire_protocol_id)
            })
            .or_else(|| {
                self.endpoints.iter().find(|endpoint| {
                    endpoint.kind == ModelProviderEndpointKind::Api
                        && endpoint.wire_protocol_id.is_none()
                })
            })
    }

    pub fn supports(&self, protocol: WireProtocol) -> bool {
        self.endpoint_for(protocol.wire_protocol_id()).is_some()
    }
}

/// Everything the gateway needs to answer one request, read fresh.
#[derive(Debug, Clone)]
pub struct GatewaySnapshot {
    pub settings: ModelGatewaySettings,
    pub upstreams: Vec<GatewayUpstream>,
    pub gateway_used_tokens: u64,
    /// Enabled routes, by id. A request that names `route/<id>` resolves here.
    pub routes: BTreeMap<String, GatewayRoute>,
    /// Enabled provider groups, by id.
    ///
    /// Kept apart from `routes` because the two are addressed differently: a
    /// route is one model, a group is a set of models. Merging them would make
    /// every caller ask which shape it holds.
    pub groups: BTreeMap<String, GatewayProviderGroup>,
}

/// One provider group as the gateway serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProviderGroup {
    pub group_id: String,
    pub display_name: String,
    pub strategy: vibex_core::RouteStrategy,
    pub first_token_timeout_ms: Option<u64>,
    pub pin: vibex_core::GatewayPinScope,
    pub token_limit: Option<vibex_core::GatewayTokenLimit>,
    /// The Providers in attempt order, already filtered to the ones that exist.
    pub providers: Vec<crate::route::RouteMemberRef>,
    /// The models every Provider serves, and which Providers serve each.
    pub models: Vec<GatewayGroupModel>,
}

/// One model a provider group offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayGroupModel {
    pub model: String,
    /// The Providers serving the model, in the group's attempt order.
    pub providers: Vec<ModelProviderProfileId>,
}

impl GatewayProviderGroup {
    /// The id a request names for one of this group's models.
    pub fn model_id(&self, model: &str) -> String {
        format!(
            "{}{}/{}",
            vibex_core::PROVIDER_GROUP_MODEL_PREFIX,
            self.group_id,
            model
        )
    }

    /// The model this request names, when the request names this group.
    pub fn model_of<'a>(&self, requested: &'a str) -> Option<&'a str> {
        vibex_core::provider_group_model_ref(requested)
            .filter(|parsed| parsed.group_id == self.group_id)
            .map(|parsed| parsed.model_id)
    }

    /// The Providers that may answer for one model.
    pub fn providers_for(&self, model: &str) -> Vec<ModelProviderProfileId> {
        self.models
            .iter()
            .find(|entry| entry.model == model)
            .map(|entry| entry.providers.clone())
            .unwrap_or_default()
    }
}

/// One route as the gateway serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub route_id: String,
    pub display_name: String,
    pub strategy: vibex_core::RouteStrategy,
    pub first_token_timeout_ms: Option<u64>,
    /// Whether this route holds a conversation on one Key.
    pub pin: vibex_core::GatewayPinScope,
    /// A ceiling on the tokens this route may spend in its window.
    pub token_limit: Option<vibex_core::GatewayTokenLimit>,
    /// Members in attempt order, already filtered to enabled ones.
    pub members: Vec<crate::route::RouteMemberRef>,
}

impl GatewayRoute {
    /// The route's model id, as an Agent asks for it.
    pub fn model_id(&self) -> String {
        format!("{}{}", crate::route::ROUTE_MODEL_PREFIX, self.route_id)
    }
}

impl GatewaySnapshot {
    pub fn route(&self, route_id: &str) -> Option<&GatewayRoute> {
        self.routes.get(route_id)
    }

    pub fn route_ids(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }

    /// The group a model id names, when it names a model the group offers.
    ///
    /// The membership check is what makes the intersection real: a model only
    /// one of the group's Providers serves is not addressable through the
    /// group, and resolving it would let the strategy pick a Provider that has
    /// never heard of it.
    pub fn group_for_model<'a>(
        &'a self,
        requested: &'a str,
    ) -> Option<(&'a GatewayProviderGroup, &'a str)> {
        let parsed = vibex_core::provider_group_model_ref(requested)?;
        let group = self.groups.get(parsed.group_id)?;
        group
            .models
            .iter()
            .any(|model| model.model == parsed.model_id)
            .then_some((group, parsed.model_id))
    }

    pub fn upstream(&self, profile: &ModelProviderProfileId) -> Option<&GatewayUpstream> {
        self.upstreams
            .iter()
            .find(|upstream| &upstream.profile_id == profile)
    }

    pub fn models_by_upstream(&self, profile: &ModelProviderProfileId) -> Vec<String> {
        self.upstream(profile)
            .map(|upstream| upstream.models.clone())
            .unwrap_or_default()
    }

    /// Every model the gateway can serve.
    ///
    /// Routes come first, one entry each: a route is one selectable model, so
    /// listing its members would show an Agent a choice the route exists to
    /// make for it. The upstream entries stay because a direct Provider is
    /// still reachable by name.
    pub fn model_catalog(&self) -> Vec<GatewayModelEntry> {
        let mut entries = self
            .routes
            .values()
            .map(|route| GatewayModelEntry {
                id: route.model_id(),
                model: route.model_id(),
                provider: ModelProviderProfileId::new(),
                provider_name: route.display_name.clone(),
            })
            .collect::<Vec<_>>();
        // A group contributes one entry per shared model rather than one entry
        // for the group: the group is not a model, and an Agent must be able to
        // name the model it wants.
        for group in self.groups.values() {
            for model in &group.models {
                entries.push(GatewayModelEntry {
                    id: group.model_id(&model.model),
                    model: model.model.clone(),
                    provider: ModelProviderProfileId::new(),
                    provider_name: group.display_name.clone(),
                });
            }
        }
        let namespaces = self.model_namespaces();
        for upstream in &self.upstreams {
            let Some(namespace) = namespaces
                .iter()
                .find(|namespace| namespace.profile == upstream.profile_id)
            else {
                continue;
            };
            for model in &upstream.models {
                entries.push(GatewayModelEntry {
                    id: format!("{}/{}", namespace.alias, model),
                    model: model.clone(),
                    provider: upstream.profile_id.clone(),
                    provider_name: upstream.display_name.clone(),
                });
            }
        }
        entries
    }

    /// The namespaces model ids are resolved against.
    ///
    /// Two upstreams may share a display name. Their short alias would then be
    /// ambiguous, so both fall back to their unique Provider Profile id: a
    /// listing must never publish an id the resolver would refuse.
    pub fn model_namespaces(&self) -> Vec<crate::route::ModelNamespace> {
        let aliases = self
            .upstreams
            .iter()
            .map(|upstream| {
                (
                    upstream.profile_id.clone(),
                    crate::route::provider_alias(&upstream.display_name),
                )
            })
            .collect::<Vec<_>>();
        self.upstreams
            .iter()
            .map(|upstream| {
                let alias = aliases
                    .iter()
                    .find(|(profile, _)| profile == &upstream.profile_id)
                    .map(|(_, alias)| alias.clone())
                    .unwrap_or_else(|| upstream.profile_id.as_str().to_string());
                let alias = if aliases
                    .iter()
                    .filter(|(_, candidate)| candidate == &alias)
                    .count()
                    > 1
                {
                    upstream.profile_id.as_str().to_string()
                } else {
                    alias
                };
                crate::route::ModelNamespace {
                    profile: upstream.profile_id.clone(),
                    alias,
                    models: upstream.models.clone(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayModelEntry {
    pub id: String,
    pub model: String,
    pub provider: ModelProviderProfileId,
    pub provider_name: String,
}

/// Limits the service applies while serving.
#[derive(Debug, Clone)]
pub struct GatewayLimits {
    /// The largest request body accepted, in bytes.
    pub max_body_bytes: usize,
    /// How long an upstream may take to send its headers.
    pub connect_timeout: std::time::Duration,
    /// How long the settings snapshot is reused before a re-read.
    pub refresh_interval: std::time::Duration,
    /// Network proxy inherited from system settings.
    pub proxy_url: Option<String>,
}

impl Default for GatewayLimits {
    fn default() -> Self {
        Self {
            // A coding Agent sends a long transcript, occasionally with images.
            max_body_bytes: 64 * 1024 * 1024,
            connect_timeout: std::time::Duration::from_secs(120),
            refresh_interval: std::time::Duration::from_secs(5),
            proxy_url: None,
        }
    }
}

/// Where the gateway sends request events.
///
/// A boxed callback rather than a channel, so the gateway crate needs no
/// knowledge of the runtime's event bus: the runtime decides what a subscriber
/// is, and the gateway only says what happened.
pub type GatewayEventSink = Arc<dyn Fn(vibex_core::GatewayEvent) + Send + Sync>;

/// The listener's own state, readable by the Config Center.
pub type GatewayRuntime = Arc<RwLock<ModelGatewayRuntimeState>>;

/// The gateway service.
pub struct ModelGatewayService {
    db_path: std::path::PathBuf,
    config: ProviderConfigService,
    client: reqwest::Client,
    limits: GatewayLimits,
    runtime: GatewayRuntime,
    rotation: AtomicU64,
    /// Rotation for the choice *between* route members.
    ///
    /// It is deliberately not the Key counter: one request picks a member and
    /// then a Key, and sharing a counter would advance the Key rotation by two
    /// every time a route had more than one member.
    member_rotation: AtomicU64,
    /// Per-account concurrency and request-rate guards.
    limiter: std::sync::Arc<crate::limit::Limiter>,
    listener: Mutex<Option<ListenerHandle>>,
    snapshot: RwLock<Option<(GatewaySnapshot, std::time::Instant)>>,
    /// Set while the service is shutting down, so an in-flight request stops
    /// instead of starting a new upstream call.
    shutting_down: RwLock<bool>,
    /// Consecutive attempts that produced no usable upstream response.
    ///
    /// Reaching [`Self::DEGRADE_AFTER_FAILURES`] puts the service into
    /// `Degraded`, which lets Agents fall back to their direct Provider
    /// configuration instead of every request waiting out the same failure. One
    /// success clears the count and the state.
    consecutive_failures: AtomicU64,
    /// When the last failure happened, so a stale count does not degrade a
    /// gateway that has simply been idle.
    last_failure_at_ms: AtomicU64,
    /// Requests currently being served, newest last.
    ///
    /// Bounded, because the graph draws a handful: a long-running gateway under
    /// load would otherwise grow this without limit. The oldest entry is
    /// dropped when the bound is reached, which is also what stops one stuck
    /// request from pinning memory forever.
    inflight: RwLock<Vec<vibex_core::GatewayInflight>>,
    /// Where request events go. A sink rather than a direct broadcast so the
    /// gateway crate stays unaware of the runtime's event bus.
    events: RwLock<Option<GatewayEventSink>>,
    /// Set while one request is being let through a degraded gateway.
    ///
    /// A degraded gateway is not closed: it still forwards one request at a
    /// time so a recovered upstream can prove itself. Without that, the refusal
    /// path could never reach an upstream, no success could be recorded, and
    /// the state would only clear on a restart.
    probe_in_flight: std::sync::atomic::AtomicBool,
}

struct ListenerHandle {
    task: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    port: u16,
}

impl std::fmt::Debug for ModelGatewayService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelGatewayService")
            .field("db_path", &self.db_path)
            .field("status", &self.runtime_state().status)
            .finish()
    }
}

impl ModelGatewayService {
    /// How many consecutive upstream failures degrade the gateway.
    ///
    /// Three is enough to tell a transient blip from a broken route, and small
    /// enough that a user does not sit through many failing turns first.
    pub const DEGRADE_AFTER_FAILURES: u64 = 3;
    /// A failure this old no longer contributes to the count.
    pub const FAILURE_MEMORY_MS: i64 = 2 * 60 * 1000;
    /// The most in-flight requests retained for a client to draw.
    ///
    /// A graph that drew more would be unreadable, and a request that never
    /// finished must not pin memory indefinitely.
    pub const MAX_INFLIGHT: usize = 8;

    pub fn new(
        db_path: impl Into<std::path::PathBuf>,
        config: ProviderConfigService,
        limits: GatewayLimits,
    ) -> Arc<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(limits.connect_timeout)
            .timeout(std::time::Duration::from_secs(600))
            .user_agent("vibex-model-gateway/1");
        if let Some(proxy) = limits.proxy_url.as_deref() {
            match reqwest::Proxy::all(proxy) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(_) => tracing::warn!(
                    target: "vibex.model_gateway",
                    "ignoring an unusable proxy url for the model gateway"
                ),
            }
        }
        let client = builder.build().unwrap_or_default();
        Arc::new(Self {
            db_path: db_path.into(),
            config,
            client,
            limits,
            runtime: Arc::new(RwLock::new(ModelGatewayRuntimeState::default())),
            rotation: AtomicU64::new(0),
            member_rotation: AtomicU64::new(0),
            limiter: crate::limit::Limiter::new(),
            listener: Mutex::new(None),
            snapshot: RwLock::new(None),
            shutting_down: RwLock::new(false),
            consecutive_failures: AtomicU64::new(0),
            last_failure_at_ms: AtomicU64::new(0),
            probe_in_flight: std::sync::atomic::AtomicBool::new(false),
            inflight: RwLock::new(Vec::new()),
            events: RwLock::new(None),
        })
    }

    pub fn runtime(&self) -> GatewayRuntime {
        Arc::clone(&self.runtime)
    }

    pub fn runtime_state(&self) -> ModelGatewayRuntimeState {
        self.runtime
            .read()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    /// The base URL an Agent should point at, when the service is running.
    pub fn base_url(&self) -> Option<String> {
        self.runtime_state().base_url()
    }

    pub fn max_body_bytes(&self) -> usize {
        self.limits.max_body_bytes
    }

    pub fn database_path(&self) -> &std::path::Path {
        &self.db_path
    }

    fn connection(&self) -> GatewayResult<DbConnection> {
        let mut connection = open_database(&self.db_path).map_err(GatewayError::from)?;
        apply_migrations(&mut connection).map_err(GatewayError::from)?;
        Ok(connection)
    }

    /// Reads the durable settings, creating the row on first use.
    pub fn settings(&self) -> GatewayResult<ModelGatewaySettings> {
        let connection = self.connection()?;
        ModelGatewaySettingsRepository::get_or_create(&connection).map_err(GatewayError::from)
    }

    /// Reads a fresh snapshot of the configuration the gateway routes over.
    pub fn read_snapshot(&self) -> GatewayResult<GatewaySnapshot> {
        let connection = self.connection()?;
        let settings = ModelGatewaySettingsRepository::get_or_create(&connection)?;
        let now = vibex_core::unix_timestamp_ms();
        let profiles = ProviderConfigService::list_model_provider_profiles(&self.config)?;
        let profiles = profiles
            .into_iter()
            .filter(|profile| profile.status == ModelProviderProfileStatus::Enabled)
            .map(|profile| (profile.id.clone(), profile))
            .collect::<BTreeMap<_, _>>();

        let window = settings
            .policy
            .token_limit
            .map(|limit| limit.window)
            .unwrap_or(GatewayTokenWindow::Day);
        // A route's members must be resolvable even when they are not also on
        // the global upstream list. Requiring both would make the user add a
        // Provider twice — once as an upstream, once as a member — and the
        // second one would be invisible in the gateway's own configuration.
        let mut referenced = settings.upstreams.clone();
        let enabled_routes = vibex_db::ModelRouteRepository::list_enabled(&connection)?;
        for route in &enabled_routes {
            // A direct route is not served here, so its Providers are not
            // upstreams of this listener.
            if !route.kind.via_gateway() {
                continue;
            }
            for member in route.enabled_members() {
                if !referenced.contains(&member.model_provider_profile_id) {
                    referenced.push(member.model_provider_profile_id.clone());
                }
            }
        }

        let mut gateway_used_tokens = 0;
        let mut upstreams = Vec::with_capacity(referenced.len());
        for profile_id in &referenced {
            let Some(profile) = profiles.get(profile_id) else {
                continue;
            };
            let usage =
                GatewayTokenWindowRepository::current(&connection, profile_id, window, now)?;
            let used = usage.as_ref().map(|usage| usage.used_tokens).unwrap_or(0);
            gateway_used_tokens += used;
            let states = ModelProviderAccountStateRepository::list(&connection, profile_id)?;
            let state_by_key = states
                .into_iter()
                .map(|state| (state.credential_id.clone(), state))
                .collect::<BTreeMap<_, _>>();
            let keys = profile
                .credentials
                .iter()
                .map(|credential| {
                    let state = state_by_key
                        .get(&credential.id)
                        .cloned()
                        .unwrap_or_else(|| {
                            ModelProviderAccountState::ready(
                                profile_id.clone(),
                                credential.id.clone(),
                            )
                        });
                    GatewayKeySummary {
                        credential_id: credential.id.clone(),
                        display_name: credential.display_name.clone(),
                        state: state.visibility_at(now),
                        used_tokens: used,
                    }
                })
                .collect::<Vec<_>>();
            upstreams.push(GatewayUpstream {
                profile_id: profile_id.clone(),
                display_name: profile.display_name.clone(),
                endpoints: profile.endpoints.clone(),
                models: profile
                    .configured_models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(|model| model.id.clone())
                    .collect(),
                credentials: profile.credentials.clone(),
                keys,
            });
        }
        // Enabled routes are what an Agent may name. A route that points at a
        // Provider no longer in the snapshot keeps its members out: a member
        // whose upstream is gone cannot be called, and pretending otherwise
        // would fail deep inside `prepare_member`.
        let routes = enabled_routes
            .iter()
            .filter(|route| route.kind == vibex_core::RouteKind::Gateway)
            .filter_map(|route| {
                let members = route
                    .enabled_members()
                    .filter(|member| {
                        profiles.contains_key(&member.model_provider_profile_id)
                            && route.enabled_members().any(|_| true)
                    })
                    .map(|member| crate::route::RouteMemberRef {
                        profile: member.model_provider_profile_id.clone(),
                        model: member.model_id.clone(),
                        reasoning_effort: member.reasoning_effort.clone(),
                    })
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    return None;
                }
                Some((
                    route.id.as_str().to_string(),
                    GatewayRoute {
                        route_id: route.id.as_str().to_string(),
                        display_name: route.display_name.clone(),
                        strategy: route.strategy,
                        first_token_timeout_ms: route.first_token_timeout_ms,
                        pin: route.pin,
                        token_limit: route.token_limit,
                        members,
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>();

        // A provider group offers the intersection of its Providers'
        // catalogues. The models are computed here, once, from the same
        // profiles the request will be prepared against: computing them at
        // request time would let the catalogue and the router disagree about
        // which model is available.
        let group_profiles = profiles.values().cloned().collect::<Vec<_>>();
        let groups = enabled_routes
            .iter()
            .filter(|route| route.is_provider_group())
            .filter_map(|route| {
                let providers = route
                    .enabled_members()
                    .filter_map(|member| {
                        profiles
                            .get(&member.model_provider_profile_id)
                            .map(|profile| {
                                crate::route::RouteMemberRef {
                                    profile: profile.id.clone(),
                                    // A group member names a Provider, so the router
                                    // substitutes the model per request.
                                    model: String::new(),
                                    reasoning_effort: member.reasoning_effort.clone(),
                                }
                            })
                    })
                    .collect::<Vec<_>>();
                if providers.is_empty() {
                    // A group with no reachable Provider has nothing to serve.
                    // One Provider *is* useful, though: a group of one is how a
                    // single upstream gets the gateway's protocol translation
                    // without pretending there is a second source to fail over
                    // to.
                    return None;
                }
                let models = vibex_core::provider_group_models(route, &group_profiles)
                    .into_iter()
                    .map(|model| GatewayGroupModel {
                        model: model.model_id,
                        providers: model.provider_profile_ids,
                    })
                    .collect::<Vec<_>>();
                if models.is_empty() {
                    // No shared model means the group cannot answer anything,
                    // which is a broken composition rather than a usable one.
                    return None;
                }
                Some((
                    route.id.as_str().to_string(),
                    GatewayProviderGroup {
                        group_id: route.id.as_str().to_string(),
                        display_name: route.display_name.clone(),
                        strategy: route.strategy,
                        first_token_timeout_ms: route.first_token_timeout_ms,
                        pin: route.pin,
                        token_limit: route.token_limit,
                        providers,
                        models,
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>();

        Ok(GatewaySnapshot {
            settings,
            upstreams,
            gateway_used_tokens,
            routes,
            groups,
        })
    }

    pub(crate) fn cached_snapshot(&self) -> GatewayResult<GatewaySnapshot> {
        if let Ok(guard) = self.snapshot.read()
            && let Some((snapshot, read_at)) = guard.as_ref()
            && read_at.elapsed() < self.limits.refresh_interval
        {
            return Ok(snapshot.clone());
        }
        let snapshot = self.read_snapshot()?;
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = Some((snapshot.clone(), std::time::Instant::now()));
        }
        Ok(snapshot)
    }

    /// Invalidates the cached snapshot after a settings mutation.
    pub fn invalidate_snapshot(&self) {
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = None;
        }
    }

    /// Applies a settings change to the running listener.
    ///
    /// Turning automatic routing off must actually stop the listener, not just
    /// flip a flag: an Agent pointing at a dead port is the honest state, and
    /// every binding falls back to its direct Provider. Turning it back on
    /// re-binds the persisted port.
    pub async fn apply_policy(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        let settings = self.settings()?;
        let running = self.runtime_state().status == ModelGatewayStatus::Running;
        match (settings.policy.enabled, running) {
            (false, true) => {
                self.stop_listener().await;
                let state = ModelGatewayRuntimeState {
                    status: ModelGatewayStatus::Disabled,
                    notice: Some("Automatic routing is turned off".to_string()),
                    ..ModelGatewayRuntimeState::default()
                };
                self.publish(state.clone());
                Ok(state)
            }
            (true, false) => self.start().await,
            _ => {
                self.invalidate_snapshot();
                Ok(self.runtime_state())
            }
        }
    }

    /// Stops the listener but leaves the service usable, so it can be started
    /// again without rebuilding it.
    async fn stop_listener(&self) {
        let handle = self.listener.lock().ok().and_then(|mut guard| guard.take());
        if let Some(handle) = handle {
            handle.task.abort();
            let _ = handle.task.await;
        }
    }

    /// Starts the listener, persisting the port that actually bound.
    ///
    /// A port collision probes upward from the persisted port and stores the
    /// result, so the next start does not move again and Agents keep the same
    /// base URL.
    pub async fn start(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        let result = self.start_inner().await;
        if let Err(error) = &result {
            // Every start failure is a terminal state for this attempt. Without
            // this, a settings or snapshot read that failed before the bind
            // would leave the Config Center saying "starting" forever.
            let current = self.runtime_state();
            if current.status == ModelGatewayStatus::Starting {
                self.publish(ModelGatewayRuntimeState::unavailable(error.message()));
            }
        }
        result
    }

    async fn start_inner(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        if let Ok(mut shutting_down) = self.shutting_down.write() {
            *shutting_down = false;
        }
        let settings = self.settings()?;
        if !settings.policy.enabled {
            let state = ModelGatewayRuntimeState {
                status: ModelGatewayStatus::Disabled,
                notice: Some("Automatic routing is turned off".to_string()),
                ..ModelGatewayRuntimeState::default()
            };
            self.publish(state.clone());
            return Ok(state);
        }
        let snapshot = self.read_snapshot()?;
        if snapshot.upstreams.is_empty() {
            let state = ModelGatewayRuntimeState::unavailable(
                "Add at least one upstream Provider before automatic routing can work",
            );
            self.publish(state.clone());
            return Ok(state);
        }

        let listener = match self.bind(settings.port).await {
            Ok(listener) => listener,
            Err(error) => {
                // A bind failure is a terminal state, not a pending one: the
                // Config Center must stop saying "starting" and tell the user
                // that Agents are on their direct Provider.
                self.publish(ModelGatewayRuntimeState::unavailable(error.message()));
                return Err(error);
            }
        };
        let port = listener
            .local_addr()
            .map_err(|_| GatewayError::Internal("the gateway listener has no address".to_string()))?
            .port();
        if settings.port != port
            && let Ok(connection) = self.connection()
        {
            let _ = ModelGatewaySettingsRepository::store_bound_port(&connection, port);
        }
        self.invalidate_snapshot();

        let router = crate::server::build_router(Arc::clone(self));
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(
                    target: "vibex.model_gateway",
                    %error,
                    "the model gateway stopped serving"
                );
            }
        });
        if let Ok(mut guard) = self.listener.lock() {
            *guard = Some(ListenerHandle { task, port });
        }
        let state = ModelGatewayRuntimeState::running(port, snapshot.upstreams.len());
        self.publish(state.clone());
        self.prune_ledgers();
        Ok(state)
    }

    async fn bind(&self, preferred: u16) -> GatewayResult<tokio::net::TcpListener> {
        if preferred == 0 {
            return tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| self.bind_error(0, error));
        }
        let mut last_error = None;
        for offset in 0..=20u16 {
            let Some(port) = preferred.checked_add(offset) else {
                break;
            };
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => return Ok(listener),
                Err(error) => last_error = Some((port, error)),
            }
        }
        let (port, error) =
            last_error.unwrap_or((preferred, std::io::Error::other("no port available")));
        Err(self.bind_error(port, error))
    }

    fn bind_error(&self, port: u16, error: std::io::Error) -> GatewayError {
        GatewayError::NotConfigured(format!(
            "could not listen on 127.0.0.1:{port} ({error}). Agent requests fall back to their direct Provider configuration."
        ))
    }

    /// Records one upstream success, clearing the failure count and any
    /// degraded state.
    pub(crate) fn note_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.last_failure_at_ms.store(0, Ordering::Relaxed);
        let mut state = self.runtime_state();
        if state.status == ModelGatewayStatus::Degraded {
            state.status = ModelGatewayStatus::Running;
            state.notice = None;
            state.updated_at_ms = vibex_core::unix_timestamp_ms();
            self.publish(state);
        }
    }

    /// Records one failed request and degrades the gateway once the count is
    /// reached.
    ///
    /// This counts *requests the caller saw fail*, not upstream attempts: one
    /// dead upstream cools its only Key, and every later request is refused
    /// before it can attempt anything. Counting attempts would never reach the
    /// threshold in exactly the case that matters.
    ///
    /// A failure observed while the gateway is already degraded only advances
    /// the record; it never re-degrades or rewrites the notice, because
    /// [`Self::begin_attempt`] is what decides whether an upstream is even
    /// consulted.
    pub(crate) fn note_failure(&self) {
        let now = vibex_core::unix_timestamp_ms();
        let previous = self.last_failure_at_ms.swap(now as u64, Ordering::Relaxed) as i64;
        // A failure older than the memory window starts a fresh count rather
        // than adding to a tally from an unrelated outage.
        let count = if previous != 0 && now.saturating_sub(previous) > Self::FAILURE_MEMORY_MS {
            self.consecutive_failures.store(1, Ordering::Relaxed);
            1
        } else {
            self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
        };
        if count < Self::DEGRADE_AFTER_FAILURES {
            return;
        }
        let mut state = self.runtime_state();
        if state.status != ModelGatewayStatus::Running {
            return;
        }
        state.status = ModelGatewayStatus::Degraded;
        state.notice = Some(
            "Repeated upstream failures: only one request at a time is let through until an upstream answers again."
                .to_string(),
        );
        state.updated_at_ms = now;
        self.publish(state);
    }

    /// Installs the sink request events are delivered to.
    ///
    /// The runtime owns the event bus, so the gateway is handed a way to send
    /// rather than a bus of its own: one listener, one place events originate.
    pub fn set_event_sink(&self, sink: GatewayEventSink) {
        if let Ok(mut guard) = self.events.write() {
            *guard = Some(sink);
        }
    }

    /// The requests being served right now.
    ///
    /// This is what a graph reads on first paint, before any event arrives; the
    /// event stream then keeps it current.
    pub fn inflight_requests(&self) -> Vec<vibex_core::GatewayInflight> {
        self.inflight
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Records that a request started, or moved to another member.
    pub(crate) fn note_inflight(&self, inflight: vibex_core::GatewayInflight) {
        let request_id = inflight.request_id.clone();
        if let Ok(mut guard) = self.inflight.write() {
            guard.retain(|existing| existing.request_id != request_id);
            guard.push(inflight.clone());
            // Keep the newest `MAX_INFLIGHT`; a graph that cannot draw them all
            // does not need them all retained.
            while guard.len() > Self::MAX_INFLIGHT {
                guard.remove(0);
            }
        }
        self.publish_event(vibex_core::GatewayEvent::Began(inflight));
    }

    /// Records the first token of a request, which ends its deadline.
    pub(crate) fn note_first_token(&self, request_id: &RequestId, at_ms: i64) {
        let updated = {
            let Ok(mut guard) = self.inflight.write() else {
                return;
            };
            let Some(entry) = guard
                .iter_mut()
                .find(|entry| &entry.request_id == request_id)
            else {
                return;
            };
            if entry.first_token_at_ms.is_some() {
                return;
            }
            entry.first_token_at_ms = Some(at_ms);
            true
        };
        if updated {
            self.publish_event(vibex_core::GatewayEvent::FirstToken {
                request_id: request_id.clone(),
                at_ms,
            });
        }
    }

    /// Drops a finished request from the in-flight table.
    pub(crate) fn note_finished(
        &self,
        request_id: &RequestId,
        outcome: vibex_core::GatewayRequestOutcome,
    ) {
        if let Ok(mut guard) = self.inflight.write() {
            guard.retain(|entry| &entry.request_id != request_id);
        }
        self.publish_event(vibex_core::GatewayEvent::Ended {
            request_id: request_id.clone(),
            outcome,
        });
    }

    fn publish_event(&self, event: vibex_core::GatewayEvent) {
        let sink = self.events.read().ok().and_then(|guard| guard.clone());
        if let Some(sink) = sink {
            sink(event);
        }
    }

    /// Whether the gateway is currently stepping aside.
    pub fn is_degraded(&self) -> bool {
        self.runtime_state().status == ModelGatewayStatus::Degraded
    }

    /// Decides whether this request may consult an upstream, and claims the
    /// probe slot when it is the one a degraded gateway lets through.
    ///
    /// A degraded gateway is not closed. It admits exactly one request at a
    /// time: if that request reaches an upstream and succeeds,
    /// [`Self::note_success`] clears the state; if it fails, the refusal keeps
    /// every other caller on their direct Provider instead of queueing them
    /// behind a broken upstream.
    ///
    /// Returns `true` when this request claimed the probe slot and must release
    /// it with [`Self::finish_attempt`], including on every error path.
    pub(crate) fn begin_attempt(&self) -> GatewayResult<bool> {
        if !self.is_degraded() {
            return Ok(false);
        }
        if self
            .probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return Err(GatewayError::NotConfigured(Self::STEPPED_ASIDE.to_string()));
        }
        Ok(true)
    }

    /// Releases the slot [`Self::begin_attempt`] may have claimed.
    pub(crate) fn finish_attempt(&self, probe: bool) {
        if probe {
            self.probe_in_flight.store(false, Ordering::Release);
        }
    }

    /// The notice and error text a degraded gateway returns to callers it is
    /// not currently letting through. Kept in one place so the server can
    /// recognise its own refusal and never count it as an upstream failure.
    pub(crate) const STEPPED_ASIDE: &'static str = "the gateway stepped aside after repeated upstream failures; let one request through at a time until an upstream answers, or use the direct Provider";

    pub fn publish(&self, state: ModelGatewayRuntimeState) {
        if let Ok(mut guard) = self.runtime.write() {
            *guard = state;
        }
    }

    /// Stops the listener. Idempotent.
    pub async fn shutdown(&self) {
        if let Ok(mut shutting_down) = self.shutting_down.write() {
            *shutting_down = true;
        }
        self.stop_listener().await;
        let mut state = self.runtime_state();
        state.status = ModelGatewayStatus::Unavailable;
        state.port = None;
        state.notice = Some("The gateway is stopped".to_string());
        self.publish(state);
    }

    fn prune_ledgers(&self) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let _ = ModelGatewayRequestLogRepository::prune(&connection, now);
        let _ = GatewayConversationPinRepository::prune(&connection, now, 512);
    }

    /// Resolves an inbound body into a prepared upstream call.
    pub fn prepare(
        &self,
        inbound: WireProtocol,
        headers: &HeaderMap,
        body: &[u8],
    ) -> GatewayResult<PreparedCall> {
        let request = parse_request(inbound, body)?;
        let body_value: Value = serde_json::from_slice(body).map_err(|_| {
            GatewayError::MalformedRequest("the body is not valid JSON".to_string())
        })?;
        let snapshot = self.cached_snapshot()?;
        // A degraded gateway still prepares and forwards one request at a time:
        // that probe is the only way an upstream can prove it recovered, since
        // the success that clears the breaker can only be observed upstream.
        // The claim is held by a guard, so every early return below releases
        // it. It is handed to `PreparedCall` only once the request is ready to
        // be sent.
        let mut probe = ProbeGuard {
            service: self,
            armed: self.begin_attempt()?,
        };
        // A route may set its own ceiling. It is checked instead of the gateway's
        // rather than in addition: a route that names three members needs one
        // ceiling over the composition, and the per-Key limits do not compose —
        // three Keys with a limit each still allow three times the spend.
        let route_token_limit = resolved_model_route_id(&request.model).and_then(|route_id| {
            snapshot
                .route(&route_id)
                .and_then(|route| route.token_limit)
        });
        let ceiling = route_token_limit
            .as_ref()
            .or(snapshot.settings.policy.token_limit.as_ref())
            .map(|limit| limit.tokens);
        if let Some(tokens) = ceiling
            && snapshot.gateway_used_tokens >= tokens
        {
            return Err(GatewayError::TokenCeilingReached {
                provider: snapshot.settings.display_name.clone(),
            });
        }

        // Resolve the request into the list of upstreams it may call. A route
        // expands to its members; anything else is a one-member list. Both then
        // take the same path, which is what keeps a direct Provider and a
        // route from growing two sets of routing rules.
        let resolved_members = self.resolve_members(&snapshot, &request)?;
        if resolved_members.is_empty() {
            return Err(GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest: snapshot
                    .model_catalog()
                    .into_iter()
                    .map(|entry| entry.id)
                    .collect(),
            });
        }

        let policy = &snapshot.settings.policy;
        let now = vibex_core::unix_timestamp_ms();
        let connection = self.connection()?;
        let agent_id = agent_id_from_headers(headers);
        let header_pairs = headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_string(), value.to_string()))
            })
            .collect::<Vec<_>>();
        // A route may override the gateway's pinning. The route's own choice wins
        // because pinning is a property of the composition: one route over
        // several Keys may want a conversation held on one of them while another
        // over the same Providers prefers to re-pick each request.
        let pin = resolved_model_route_id(&request.model)
            .and_then(|route_id| snapshot.route(&route_id).map(|route| route.pin))
            .unwrap_or(policy.pin);
        let conversation = (pin == GatewayPinScope::Conversation)
            .then(|| conversation_key(agent_id.as_deref(), &header_pairs, &body_value))
            .flatten();
        let pinned = conversation
            .as_deref()
            .and_then(|key| {
                GatewayConversationPinRepository::get(&connection, key)
                    .ok()
                    .flatten()
            })
            .filter(|pin| pin.is_live_at(now))
            .map(|pin| (pin.model_provider_profile_id, pin.credential_id));

        let mut members = Vec::new();
        let mut last_error: Option<GatewayError> = None;
        for resolved in resolved_members {
            match self.prepare_member(
                &snapshot,
                &request,
                inbound,
                &resolved,
                &connection,
                pinned.as_ref(),
                now,
            ) {
                Ok(member) => members.push(member),
                // A member that cannot be called at all is skipped so the next
                // one can answer. Its reason is kept in case none can, so the
                // caller reads the real cause instead of a bare 404.
                Err(error) => last_error = Some(error),
            }
        }
        if members.is_empty() {
            return Err(last_error.unwrap_or_else(|| GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest: Vec::new(),
            }));
        }

        let prepared = PreparedCall {
            inbound,
            requested_model: request.model.clone(),
            members,
            agent_id,
            conversation,
            stream: request.stream,
            route_id: resolved_model_route_id(&request.model),
            probe: probe.armed,
            request_id: RequestId::new(),
        };
        // The slot now belongs to the caller of `open_attempt`.
        probe.armed = false;
        Ok(prepared)
    }

    /// Expands the request into the upstreams it may call, in attempt order.
    ///
    /// A `route/<id>` request becomes the route's enabled members, ordered by
    /// the route's strategy. Anything else resolves to one upstream, so the
    /// caller needs no special case.
    fn resolve_members(
        &self,
        snapshot: &GatewaySnapshot,
        request: &CanonicalRequest,
    ) -> GatewayResult<Vec<ResolvedMember>> {
        if let Some((group, model)) = snapshot.group_for_model(&request.model) {
            // A group is addressed by `group/<id>/<model>`: the model is part
            // of the id because the group itself is not a model. Only the
            // Providers that serve this model are candidates, so the group's
            // strategy never picks one that has never heard of it.
            let model = model.to_string();
            let mut candidates = group
                .providers
                .iter()
                .filter(|provider| {
                    group
                        .providers_for(&model)
                        .iter()
                        .any(|serving| serving == &provider.profile)
                })
                .cloned()
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return Err(GatewayError::ModelNotFound {
                    model: request.model.clone(),
                    closest: group
                        .models
                        .iter()
                        .map(|entry| group.model_id(&entry.model))
                        .collect(),
                });
            }
            let plans = crate::route::plan_members_with_usage(
                &candidates,
                group.strategy,
                self.member_rotation.fetch_add(1, Ordering::Relaxed),
                &self.member_usage(snapshot),
            );
            for candidate in &mut candidates {
                if let Some(plan) = plans.iter().find(|plan| plan.profile == candidate.profile) {
                    candidate.reasoning_effort = plan.reasoning_effort.clone();
                }
            }
            return Ok(plans
                .into_iter()
                .map(|member| ResolvedMember {
                    profile: member.profile,
                    model: model.clone(),
                    reasoning_effort: member.reasoning_effort,
                    first_token_timeout_ms: group.first_token_timeout_ms,
                })
                .collect());
        }
        if let Some(route_id) = resolved_model_route_id(&request.model) {
            let Some(route) = snapshot.route(&route_id) else {
                return Err(GatewayError::ModelNotFound {
                    model: request.model.clone(),
                    closest: snapshot
                        .route_ids()
                        .into_iter()
                        .map(|id| format!("{}{}", crate::route::ROUTE_MODEL_PREFIX, id))
                        .collect(),
                });
            };
            let strategy = crate::route::plan_members_with_usage(
                &route.members,
                route.strategy,
                self.member_rotation.fetch_add(1, Ordering::Relaxed),
                &self.member_usage(snapshot),
            );
            return Ok(strategy
                .into_iter()
                .map(|member| ResolvedMember {
                    profile: member.profile,
                    model: member.model,
                    reasoning_effort: member.reasoning_effort,
                    first_token_timeout_ms: route.first_token_timeout_ms,
                })
                .collect());
        }

        let namespaces = snapshot.model_namespaces();
        let resolved = resolve_model(&request.model, &namespaces);
        let (profile_id, model) = match resolved {
            ResolvedModel::Qualified { profile, model }
            | ResolvedModel::Bare { profile, model } => (profile, model.to_string()),
            ResolvedModel::Ambiguous { candidates, model } => {
                // A bare id served by several upstreams resolves to the first
                // one in the user's order; the ledger records which one.
                let first =
                    candidates
                        .first()
                        .cloned()
                        .ok_or_else(|| GatewayError::ModelNotFound {
                            model: model.to_string(),
                            closest: Vec::new(),
                        })?;
                (first, model.to_string())
            }
            ResolvedModel::Route { .. } => unreachable!("a route id returns above"),
            ResolvedModel::NotFound { requested, closest } => {
                return Err(GatewayError::ModelNotFound {
                    model: requested,
                    closest,
                });
            }
        };
        Ok(vec![ResolvedMember {
            profile: profile_id,
            model,
            reasoning_effort: None,
            // A direct Provider has no route to set a deadline; the Agent's own
            // client timeout is the only bound.
            first_token_timeout_ms: None,
        }])
    }

    /// Tokens each upstream has spent in the current accounting window.
    ///
    /// This is the same figure the token ceiling uses, read once per request.
    /// A Provider missing from the list has spent nothing, which is how a newly
    /// added member gets tried.
    fn member_usage(&self, snapshot: &GatewaySnapshot) -> Vec<(ModelProviderProfileId, u64)> {
        snapshot
            .upstreams
            .iter()
            .map(|upstream| {
                let used = upstream
                    .keys
                    .first()
                    .map(|key| key.used_tokens)
                    .unwrap_or(0);
                (upstream.profile_id.clone(), used)
            })
            .collect()
    }

    /// Resolves one member into everything needed to call it.
    #[allow(clippy::too_many_arguments)]
    fn prepare_member(
        &self,
        snapshot: &GatewaySnapshot,
        request: &CanonicalRequest,
        inbound: WireProtocol,
        resolved: &ResolvedMember,
        connection: &DbConnection,
        pinned: Option<&(ModelProviderProfileId, RequestId)>,
        now: i64,
    ) -> GatewayResult<PreparedMember> {
        let profile_id = resolved.profile.clone();
        let upstream =
            snapshot
                .upstream(&profile_id)
                .cloned()
                .ok_or_else(|| GatewayError::ModelNotFound {
                    model: request.model.clone(),
                    closest: Vec::new(),
                })?;

        if request.has_vendor_file_image() && !upstream.supports(WireProtocol::Responses) {
            return Err(GatewayError::UnsupportedImageReference);
        }
        let (target, passthrough) = choose_target(inbound, &upstream, &snapshot.settings.policy)?;
        let endpoint = upstream
            .endpoint_for(target.wire_protocol_id())
            .cloned()
            .ok_or_else(|| GatewayError::TranslationDisabled {
                wire_protocol_id: target.wire_protocol_id().to_string(),
            })?;

        let policy = &snapshot.settings.policy;
        let states = ModelProviderAccountStateRepository::list(connection, &profile_id)?;
        let state_by_key = states
            .into_iter()
            .map(|state| (state.credential_id.clone(), state))
            .collect::<BTreeMap<_, _>>();

        let mut candidates = Vec::new();
        let preferred = policy.preferred_keys.get(&profile_id);
        for (index, credential) in upstream.credentials.iter().enumerate() {
            let state = state_by_key
                .get(&credential.id)
                .cloned()
                .unwrap_or_else(|| {
                    ModelProviderAccountState::ready(profile_id.clone(), credential.id.clone())
                });
            // A preferred Key leads the pool; every other Key keeps the
            // position the Provider gave it.
            let mut candidate =
                CandidateKey::new(profile_id.clone(), credential.id.clone(), index as i64);
            if preferred == Some(&credential.id) {
                candidate = candidate.preferred();
            }
            if let Some(limit) = policy.token_limit {
                candidate = candidate.with_token_limit(limit, snapshot.gateway_used_tokens);
            }
            candidate = match state.visibility_at(now) {
                vibex_core::GatewayKeyState::Ready => candidate,
                vibex_core::GatewayKeyState::Cooling { kind, .. } => candidate.cooling(kind),
                vibex_core::GatewayKeyState::Invalid => candidate.cooling(GatewayFailureKind::Auth),
                vibex_core::GatewayKeyState::Disabled => {
                    candidate.cooling(GatewayFailureKind::Other)
                }
            };
            candidates.push((candidate, credential.clone()));
        }
        if candidates.is_empty() {
            return Err(GatewayError::NotConfigured(format!(
                "`{}` has no API Key",
                upstream.display_name
            )));
        }

        // A pin only applies to the Provider it names. Another member of the
        // same route must not inherit it, or a conversation would land on a Key
        // that belongs to a different vendor.
        let member_pin = pinned
            .filter(|(profile, _)| profile == &profile_id)
            .cloned();
        let plan = plan_route(RouteRequest {
            candidates: candidates
                .iter()
                .map(|(candidate, _)| candidate.clone())
                .collect(),
            strategy: policy.routing,
            rotation: self.rotation.fetch_add(1, Ordering::Relaxed),
            pinned: member_pin,
        });
        if plan.attemptable().count() == 0 {
            return Err(GatewayError::AllKeysCooling {
                model: request.model.clone(),
                retry_after_ms: None,
            });
        }
        let keys = plan
            .attemptable()
            .filter_map(|candidate| {
                candidates
                    .iter()
                    .find(|(existing, _)| existing.credential == candidate.credential)
                    .cloned()
            })
            .collect::<Vec<_>>();

        let upstream_stream = request.stream || !passthrough;
        // A member may pin the reasoning effort, in which case it replaces what
        // the Agent asked for rather than adding to it.
        let member_request = match resolved.reasoning_effort.as_deref() {
            Some(effort) => {
                let mut member_request = request.clone();
                member_request.reasoning = ReasoningRequest {
                    disabled: effort.eq_ignore_ascii_case("off"),
                    effort: Some(effort.to_string()),
                };
                member_request
            }
            None => request.clone(),
        };
        let body =
            build_upstream_request(target, &member_request, &resolved.model, upstream_stream)?;
        Ok(PreparedMember {
            profile_id,
            provider_name: upstream.display_name.clone(),
            model: resolved.model.clone(),
            target,
            passthrough,
            endpoint,
            body,
            keys,
            limits: policy.account_limits.clone(),
            // The ledger keeps this much of the routing decision even when the
            // first attempt answers: "the gateway had to step over a cooling
            // Key" is exactly what a later failover investigation asks for.
            skipped_cooling: plan.skipped_cooling,
            skipped_over_limit: plan.skipped_over_limit,
            reasoning_effort: resolved.reasoning_effort.clone(),
            first_token_timeout_ms: resolved.first_token_timeout_ms,
        })
    }

    /// Sends one attempt, trying the prepared candidates in order until one
    /// answers with a success status. A failure before any byte reaches the
    /// client may fail over; the caller decides whether that is still allowed.
    pub(crate) async fn open_attempt(
        &self,
        prepared: &PreparedCall,
    ) -> GatewayResult<(UpstreamAttempt, KeyReference)> {
        if self.shutting_down.read().map(|flag| *flag).unwrap_or(false) {
            self.finish_attempt(prepared.probe);
            return Err(GatewayError::NotConfigured(
                "the gateway is shutting down".to_string(),
            ));
        }
        let mut last_error: Option<GatewayError> = None;
        // Every Key this loop walks past, with the reason it did not answer.
        // This is what the ledger's route trace is built from, so it records
        // the decision rather than a formatted sentence about it.
        let mut failed_attempts: Vec<GatewayRouteAttempt> = Vec::new();
        let mut skipped_members: Vec<vibex_core::GatewaySkippedMember> = Vec::new();
        // Members are tried in the order the route (or the single upstream)
        // gave them. A member that fails before any byte reaches the client
        // hands over to the next one; once a stream has started, the caller
        // decides and this loop never retries.
        for (index, member) in prepared.members.iter().enumerate() {
            // Announce the attempt before it is sent: a client draws the dot
            // moving to this member while the call is in flight, which is the
            // only moment the information is useful.
            self.note_inflight(vibex_core::GatewayInflight {
                request_id: prepared.request_id.clone(),
                agent_id: prepared.agent_id.clone(),
                model_route_id: prepared.route_id.clone(),
                requested_model: prepared.requested_model.clone(),
                model_provider_profile_id: member.profile_id.clone(),
                // One-based: the graph numbers sources as "1st, 2nd".
                attempt: (index as u32).saturating_add(1),
                credential_id: member
                    .keys
                    .first()
                    .map(|(_, credential)| credential.id.clone()),
                wire_protocol_id: member.target.wire_protocol_id().to_string(),
                started_at_ms: vibex_core::unix_timestamp_ms(),
                first_token_at_ms: None,
            });
            match self
                .open_member_attempt(prepared, member, &mut failed_attempts, &mut skipped_members)
                .await
            {
                Ok((response, target, passthrough, credential)) => {
                    self.note_success();
                    self.clear_failure(&member.profile_id, &credential);
                    return Ok((
                        UpstreamAttempt {
                            response,
                            target,
                            passthrough,
                            profile_id: member.profile_id.clone(),
                            failed_attempts: failed_attempts.clone(),
                            skipped_members: skipped_members.clone(),
                        },
                        KeyReference { id: credential },
                    ));
                }
                Err(error) => {
                    // A member that could not answer is recorded so the ledger
                    // can explain the walk. Only a failure before the reply
                    // started may fail over, which is what `open_attempt`'s
                    // caller enforces; here every error is pre-reply.
                    skipped_members.push(vibex_core::GatewaySkippedMember {
                        model_provider_profile_id: member.profile_id.clone(),
                        wire_protocol_id: member.target.wire_protocol_id().to_string(),
                        failure: member_failure_kind(&error),
                    });
                    let can_try_next = prepared.may_try_next(index);
                    if !can_try_next {
                        last_error = Some(error);
                        break;
                    }
                    last_error = Some(error);
                }
            }
        }
        self.finish_attempt(prepared.probe);
        self.note_finished(
            &prepared.request_id,
            vibex_core::GatewayRequestOutcome::Failed,
        );
        Err(last_error.unwrap_or_else(|| GatewayError::AllKeysCooling {
            model: prepared.requested_model.clone(),
            retry_after_ms: None,
        }))
    }

    /// Tries every Key of one member until one answers.
    ///
    /// Returns the response and the credential that produced it. The caller
    /// owns the decision to move to another member, so this function never
    /// retries across members itself.
    async fn open_member_attempt(
        &self,
        prepared: &PreparedCall,
        member: &PreparedMember,
        attempts: &mut Vec<GatewayRouteAttempt>,
        _members: &mut Vec<vibex_core::GatewaySkippedMember>,
    ) -> GatewayResult<(reqwest::Response, WireProtocol, bool, RequestId)> {
        let mut last_error: Option<GatewayError> = None;
        for (candidate, credential) in member.keys.iter() {
            let secret = match resolve_key_secret(credential) {
                Ok(Some(secret)) => secret,
                Ok(None) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Auth,
                    });
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key",
                        member.provider_name
                    )));
                    continue;
                }
                Err(error) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Auth,
                    });
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key: {}",
                        member.provider_name, error.code
                    )));
                    continue;
                }
            };
            // Hold this account's slot for the whole attempt, and take its
            // minute before anything leaves. A permit is released on drop, so a
            // cancelled or failed request cannot leak capacity.
            let limits = member
                .limits
                .get(&candidate.credential)
                .copied()
                .unwrap_or_default();
            let _permit = match self
                .limiter
                .acquire(
                    candidate.credential.as_str(),
                    limits.max_concurrency,
                    limits.queue_limit,
                    limits.queue_wait(),
                    std::time::Instant::now(),
                )
                .await
            {
                Ok(permit) => permit,
                Err(error) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Rate,
                    });
                    last_error = Some(error);
                    continue;
                }
            };
            if let Err(error) = self
                .limiter
                .reserve_minute(
                    candidate.credential.as_str(),
                    limits.max_rpm,
                    limits.queue_wait(),
                )
                .await
            {
                attempts.push(GatewayRouteAttempt {
                    credential_id: candidate.credential.clone(),
                    failure: GatewayFailureKind::Rate,
                });
                last_error = Some(error);
                continue;
            }
            let url = endpoint_url(
                &member.endpoint.url,
                member.target,
                &member.model,
                prepared.stream,
            )?;
            let mut wire = apply_auth(
                self.client.post(&url).json(&member.body),
                member.target,
                &secret,
            );
            if let Some(beta) = beta_header(&member.target, &member.body) {
                wire = wire.header("anthropic-beta", beta);
            }
            // A stalled vendor may not answer the response headers at all. The
            // route's deadline has to bound that wait too, or the deadline only
            // covers the stream and the request hangs before it starts.
            let sent = match member.first_token_timeout_ms {
                Some(timeout) => match tokio::time::timeout(
                    std::time::Duration::from_millis(timeout),
                    wire.send(),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        attempts.push(GatewayRouteAttempt {
                            credential_id: candidate.credential.clone(),
                            failure: GatewayFailureKind::Upstream,
                        });
                        last_error = Some(GatewayError::Upstream {
                            status: 504,
                            message: format!(
                                "{} did not answer within {} ms",
                                member.provider_name, timeout
                            ),
                        });
                        continue;
                    }
                },
                None => wire.send().await,
            };
            let response = match sent {
                Ok(response) => response,
                Err(error) => {
                    let kind = if error.is_timeout() {
                        GatewayFailureKind::Upstream
                    } else {
                        GatewayFailureKind::Proxy
                    };
                    self.mark_failure(&member.profile_id, &candidate.credential, kind);
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: kind,
                    });
                    last_error = Some(GatewayError::Upstream {
                        status: 502,
                        message: format!("{}: {}", member.provider_name, redact_transport(&error)),
                    });
                    continue;
                }
            };
            if response.status().is_success() {
                return Ok((
                    response,
                    member.target,
                    member.passthrough,
                    candidate.credential.clone(),
                ));
            }
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            let kind = classify_failure(status, &body);
            self.mark_failure(&member.profile_id, &candidate.credential, kind);
            attempts.push(GatewayRouteAttempt {
                credential_id: candidate.credential.clone(),
                failure: kind,
            });
            last_error = Some(GatewayError::Upstream {
                status,
                message: upstream_message(&body),
            });
            if !kind.should_fail_over() {
                break;
            }
        }
        Err(last_error.unwrap_or_else(|| GatewayError::AllKeysCooling {
            model: prepared.requested_model.clone(),
            retry_after_ms: None,
        }))
    }

    fn mark_failure(
        &self,
        profile: &ModelProviderProfileId,
        credential: &RequestId,
        kind: GatewayFailureKind,
    ) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let mut state = ModelProviderAccountStateRepository::list(&connection, profile)
            .unwrap_or_default()
            .into_iter()
            .find(|state| &state.credential_id == credential)
            .unwrap_or_else(|| {
                ModelProviderAccountState::ready(profile.clone(), credential.clone())
            });
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.updated_at_ms = now;
        match KeyHealth::from_failure(kind, now) {
            KeyHealth::Invalid => {
                state.cooldown_reason = Some(GatewayFailureKind::Auth);
                state.cooldown_until_ms = None;
            }
            KeyHealth::Cooling { kind, until_ms } => {
                state.cooldown_reason = Some(kind);
                state.cooldown_until_ms = Some(until_ms);
            }
            KeyHealth::Ready => {
                state.cooldown_reason = None;
                state.cooldown_until_ms = None;
            }
        }
        let _ = ModelProviderAccountStateRepository::upsert(&connection, &state);
    }

    fn clear_failure(&self, profile: &ModelProviderProfileId, credential: &RequestId) {
        if let Ok(connection) = self.connection() {
            let _ = ModelProviderAccountStateRepository::clear_cooldown(
                &connection,
                profile,
                credential,
            );
        }
    }

    /// Records one finished request in the ledger and in the token window.
    pub(crate) fn record(&self, prepared: &PreparedCall, outcome: &RequestOutcome) {
        // Every finished request lands here, including the ones whose reply was
        // cut. Releasing the probe first keeps a ledger write failure from
        // wedging the one slot a degraded gateway has.
        self.finish_attempt(prepared.probe);
        // The in-flight table and the event stream are cleared here rather than
        // at each return site: this is the single funnel every finished request
        // passes through, so nothing can be left drawn as still running.
        self.note_finished(&prepared.request_id, outcome.kind);
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        // A request that answered names the member that did; one that never
        // reached an upstream records the first member it would have tried, so
        // the row is still attributable to something the user can find.
        let answered = outcome.profile_id.clone().or_else(|| {
            prepared
                .first_member()
                .map(|member| member.profile_id.clone())
        });
        let wire_out = outcome
            .target
            .or_else(|| prepared.first_member().map(|member| member.target));
        let log = ModelGatewayRequestLog {
            request_id: prepared.request_id.clone(),
            agent_id: prepared.agent_id.clone(),
            model_provider_profile_id: answered.clone().unwrap_or_else(|| {
                // A request that matched no member at all has no Provider to
                // blame; the ledger requires one, so the requested model stands
                // in as an unreadable id.
                ModelProviderProfileId::new()
            }),
            credential_id: outcome.credential.clone(),
            model_requested: prepared.requested_model.clone(),
            model_served: outcome.served.clone(),
            wire_in: prepared.inbound.wire_protocol_id().to_string(),
            wire_out: wire_out
                .unwrap_or(WireProtocol::ChatCompletions)
                .wire_protocol_id()
                .to_string(),
            status: outcome.status,
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cached_read_tokens: outcome.usage.cached_read_tokens,
            cached_write_tokens: outcome.usage.cached_write_tokens,
            reasoning_tokens: outcome.usage.reasoning_tokens,
            latency_ms: outcome.latency_ms,
            outcome: outcome.kind,
            route: Some(GatewayRouteTrace {
                failed_attempts: outcome.failed_attempts.clone(),
                skipped_members: outcome.skipped_members.clone(),
                ..GatewayRouteTrace::new(outcome.skipped_cooling, outcome.skipped_over_limit)
            }),
            created_at_ms: now,
        };
        let _ = ModelGatewayRequestLogRepository::insert(&connection, &log);
        if outcome.usage.total() > 0
            && let Some(profile_id) = answered.as_ref()
            && let Ok(settings) = ModelGatewaySettingsRepository::get_or_create(&connection)
            && let Some(limit) = settings.policy.token_limit
        {
            let _ = GatewayTokenWindowRepository::record(
                &connection,
                profile_id,
                limit.window,
                outcome.usage.total(),
                now,
            );
        }
    }

    /// Lists the most recent ledger rows.
    pub fn recent_requests(&self, limit: usize) -> GatewayResult<Vec<ModelGatewayRequestLog>> {
        let connection = self.connection()?;
        ModelGatewayRequestLogRepository::recent(&connection, limit).map_err(GatewayError::from)
    }

    /// Clears the request ledger.
    pub fn clear_requests(&self) -> GatewayResult<usize> {
        let connection = self.connection()?;
        ModelGatewayRequestLogRepository::clear(&connection).map_err(GatewayError::from)
    }

    /// Stores a conversation pin.
    pub(crate) fn pin_conversation(
        &self,
        key: &str,
        profile: &ModelProviderProfileId,
        credential: &RequestId,
    ) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let _ = GatewayConversationPinRepository::upsert(
            &connection,
            &GatewayConversationPin {
                conversation_key: key.to_string(),
                model_provider_profile_id: profile.clone(),
                credential_id: credential.clone(),
                chosen_at_ms: now,
                expires_at_ms: now + GatewayConversationPin::TTL_MS,
            },
        );
    }

    /// Adds tokens to one Provider's window.
    pub fn record_tokens(
        &self,
        profile: &ModelProviderProfileId,
        window: GatewayTokenWindow,
        tokens: u64,
    ) -> GatewayResult<GatewayTokenWindowUsage> {
        let connection = self.connection()?;
        GatewayTokenWindowRepository::record(
            &connection,
            profile,
            window,
            tokens,
            vibex_core::unix_timestamp_ms(),
        )
        .map_err(GatewayError::from)
    }

    /// The reply a client receives when the service itself refuses a request.
    pub fn error_response(&self, protocol: WireProtocol, error: &GatewayError) -> Response {
        let message = error.message();
        let body = match protocol {
            WireProtocol::ChatCompletions | WireProtocol::Responses => serde_json::json!({
                "error": {"message": message, "type": error.code(), "code": error.code()}
            }),
            WireProtocol::AnthropicMessages => serde_json::json!({
                "type": "error",
                "error": {"type": error.code(), "message": message},
            }),
            WireProtocol::GeminiGenerateContent => serde_json::json!({
                "error": {"code": error.status(), "message": message, "status": error.code()}
            }),
        };
        let mut response = Response::new(Body::from(body.to_string()));
        *response.status_mut() =
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        if let Some(retry_after) = error.retry_after_ms() {
            let seconds = (retry_after / 1000).max(1).to_string();
            if let Ok(value) = header::HeaderValue::from_str(&seconds) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

/// Releases a degraded gateway's probe slot unless the request is handed on.
///
/// `prepare` can leave through a dozen early returns. Holding the slot in a
/// value that releases on drop means none of them has to remember to do it.
struct ProbeGuard<'a> {
    service: &'a ModelGatewayService,
    armed: bool,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.service.finish_attempt(true);
        }
    }
}

/// One upstream the request may call, before its Keys are planned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedMember {
    profile: ModelProviderProfileId,
    model: String,
    reasoning_effort: Option<String>,
    first_token_timeout_ms: Option<u64>,
}

/// The route id a model id names, if it names one.
fn resolved_model_route_id(model: &str) -> Option<String> {
    crate::route::route_id_from_model(model)
        .flatten()
        .map(str::to_string)
}

/// A request that has been resolved but not yet sent.
#[derive(Clone)]
pub struct PreparedCall {
    pub inbound: WireProtocol,
    pub requested_model: String,
    /// The members to try, in attempt order. A direct Provider is a one-member
    /// list; a route may hold several. Everything downstream walks this list,
    /// so a route and a direct Provider take the same path.
    pub members: Vec<PreparedMember>,
    pub agent_id: Option<String>,
    pub conversation: Option<String>,
    pub stream: bool,
    /// The route that produced this call, when the request named one.
    pub route_id: Option<String>,
    /// This request holds a degraded gateway's single probe slot and must
    /// release it once an upstream has answered or refused.
    pub probe: bool,
    /// The id this request is traced and recorded under.
    ///
    /// One id for the whole request, not one per attempt: the inflight entry,
    /// the ledger row, and the events a client draws all have to name the same
    /// thing.
    pub request_id: RequestId,
}

/// One upstream this request may call, with everything needed to call it.
///
/// Each member is resolved independently: it has its own endpoint, its own
/// protocol pair, and its own Key order. Two members of one route may speak
/// different protocols to different vendors, so nothing here can be shared.
#[derive(Debug, Clone)]
pub struct PreparedMember {
    pub profile_id: ModelProviderProfileId,
    pub provider_name: String,
    /// The model id this member's upstream is asked for.
    pub model: String,
    pub target: WireProtocol,
    pub passthrough: bool,
    pub endpoint: ModelProviderEndpoint,
    pub body: Value,
    /// Ordered attempts: the candidate and the credential record it uses.
    pub keys: Vec<(CandidateKey, ModelProviderCredentialReference)>,
    /// Per-credential concurrency and rate limits.
    pub limits: BTreeMap<RequestId, vibex_core::GatewayAccountLimits>,
    /// Keys the plan set aside before the first attempt, and why.
    pub skipped_cooling: usize,
    pub skipped_over_limit: usize,
    /// Reasoning effort this member asks for, overriding the Agent's request.
    pub reasoning_effort: Option<String>,
    /// How long this attempt may take to produce its first token.
    pub first_token_timeout_ms: Option<u64>,
}

impl PreparedCall {
    /// The member that should answer first.
    pub fn first_member(&self) -> Option<&PreparedMember> {
        self.members.first()
    }

    /// Whether a failure on this member may move to the next one.
    pub fn may_try_next(&self, index: usize) -> bool {
        index + 1 < self.members.len()
    }

    /// The member belonging to a Provider, once one has answered.
    pub fn member(&self, profile: &ModelProviderProfileId) -> Option<&PreparedMember> {
        self.members
            .iter()
            .find(|member| &member.profile_id == profile)
    }
}

impl std::fmt::Debug for PreparedCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCall")
            .field("inbound", &self.inbound)
            .field("requested_model", &self.requested_model)
            .field("members", &self.members.len())
            .field("agent_id", &self.agent_id)
            .field("route_id", &self.route_id)
            .field("stream", &self.stream)
            .finish()
    }
}

/// What one finished request produced, as the ledger records it.
pub(crate) struct RequestOutcome {
    pub kind: GatewayRequestOutcome,
    /// The Provider that answered. Empty when nothing answered.
    pub profile_id: Option<ModelProviderProfileId>,
    /// The protocol the answering member was called with.
    pub target: Option<WireProtocol>,
    /// Keys set aside before the first attempt of the answering member.
    pub skipped_cooling: usize,
    pub skipped_over_limit: usize,
    pub credential: Option<RequestId>,
    pub status: Option<u16>,
    pub usage: CanonicalUsage,
    pub latency_ms: Option<i64>,
    /// The model the upstream said it served, when it said one.
    pub served: Option<String>,
    /// Keys tried before the answering one, in order.
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members walked past before one answered.
    pub skipped_members: Vec<vibex_core::GatewaySkippedMember>,
}

/// The credential a request authenticates with.
///
/// Only the id is kept: the resolved Secret exists for the duration of one
/// outbound request and never reaches a log line, a ledger row, or an error.
pub(crate) struct KeyReference {
    pub id: RequestId,
}

pub(crate) struct UpstreamAttempt {
    pub response: reqwest::Response,
    pub target: WireProtocol,
    pub passthrough: bool,
    /// The member that answered.
    pub profile_id: ModelProviderProfileId,
    /// Keys that did not answer before this one did.
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members walked past before this one answered.
    pub skipped_members: Vec<vibex_core::GatewaySkippedMember>,
}

/// Chooses the protocol the upstream is called with.
///
/// Passthrough wins whenever the upstream speaks what the client spoke: a
/// same-protocol call is forwarded byte for byte, which is what keeps a
/// vendor's prompt cache valid.
fn choose_target(
    inbound: WireProtocol,
    upstream: &GatewayUpstream,
    policy: &GatewayPolicy,
) -> GatewayResult<(WireProtocol, bool)> {
    if upstream.supports(inbound) {
        return Ok((inbound, true));
    }
    let preference = [
        // Anthropic first: the translation into Messages keeps cache
        // breakpoints and thinking signatures, which the others cannot carry.
        WireProtocol::AnthropicMessages,
        WireProtocol::ChatCompletions,
        WireProtocol::Responses,
        WireProtocol::GeminiGenerateContent,
    ];
    for protocol in preference {
        if upstream.supports(protocol) && policy.translate.allows(protocol.wire_protocol_id()) {
            return Ok((protocol, false));
        }
    }
    Err(GatewayError::TranslationDisabled {
        wire_protocol_id: inbound.wire_protocol_id().to_string(),
    })
}

/// How a member-level failure should be described in the route trace.
///
/// The distinction the ledger needs is "was this the Key or the whole source".
/// An upstream error keeps its classified kind so the trace still says "rate
/// limited" rather than a generic failure.
fn member_failure_kind(error: &GatewayError) -> GatewayFailureKind {
    match error {
        GatewayError::Upstream { status, message } => classify_failure(*status, message),
        GatewayError::AllKeysCooling { .. } => GatewayFailureKind::Rate,
        GatewayError::TokenCeilingReached { .. } => GatewayFailureKind::Quota,
        GatewayError::NotConfigured(_) => GatewayFailureKind::Auth,
        GatewayError::TranslationDisabled { .. }
        | GatewayError::UnsupportedImageReference
        | GatewayError::MalformedRequest(_) => GatewayFailureKind::Other,
        GatewayError::ModelNotFound { .. } | GatewayError::Internal(_) => GatewayFailureKind::Other,
    }
}

fn agent_id_from_headers(headers: &HeaderMap) -> Option<String> {
    for name in ["x-vibex-agent", "x-agent-id", "user-agent"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let value = value.trim();
            if !value.is_empty() && value.len() <= 64 && !value.contains('/') {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn resolve_key_secret(
    credential: &ModelProviderCredentialReference,
) -> vibex_core::VibexResult<Option<String>> {
    let Some(reference) = credential.credential.secret_reference() else {
        return Ok(None);
    };
    secrets::resolve_provider_secret_reference(
        reference.backend,
        reference.setup_state,
        &reference.lookup_key,
    )
}

fn apply_auth(
    request: reqwest::RequestBuilder,
    protocol: WireProtocol,
    secret: &str,
) -> reqwest::RequestBuilder {
    match protocol {
        WireProtocol::ChatCompletions | WireProtocol::Responses => request.bearer_auth(secret),
        WireProtocol::AnthropicMessages => request
            .header("x-api-key", secret)
            .header("anthropic-version", "2023-06-01"),
        WireProtocol::GeminiGenerateContent => request.header("x-goog-api-key", secret),
    }
}

/// Anthropic requires the 1-hour cache TTL to be opted into with a beta header.
fn beta_header(protocol: &WireProtocol, body: &Value) -> Option<&'static str> {
    if *protocol != WireProtocol::AnthropicMessages {
        return None;
    }
    body.to_string()
        .contains("\"ttl\":\"1h\"")
        .then_some("extended-cache-ttl-2025-04-11")
}

/// Joins an endpoint base with a protocol path without doubling a version
/// prefix the user already typed into the address.
fn endpoint_url(
    base: &str,
    protocol: WireProtocol,
    model: &str,
    stream: bool,
) -> GatewayResult<String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(GatewayError::NotConfigured(
            "the upstream Provider has an empty address".to_string(),
        ));
    }
    let path = protocol.path(model, stream);
    for prefix in ["/v1beta", "/v1"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return Ok(if base.ends_with(prefix) {
                format!("{base}{rest}")
            } else {
                format!("{base}{path}")
            });
        }
    }
    Ok(format!("{base}{path}"))
}

fn upstream_message(body: &str) -> String {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for pointer in [
            "/error/message",
            "/error/msg",
            "/message",
            "/error",
            "/detail",
        ] {
            if let Some(message) = value.pointer(pointer).and_then(Value::as_str) {
                return truncate(message);
            }
        }
    }
    if trimmed.is_empty() {
        "the upstream refused the request without a message".to_string()
    } else {
        truncate(trimmed)
    }
}

fn truncate(value: &str) -> String {
    const MAX: usize = 512;
    if value.chars().count() <= MAX {
        return value.to_string();
    }
    let mut truncated = value.chars().take(MAX).collect::<String>();
    truncated.push('…');
    truncated
}

fn redact_transport(error: &reqwest::Error) -> String {
    // A transport error can carry the full URL, including a query string some
    // vendors put a Key into. Only the reason is reported.
    if error.is_timeout() {
        "the upstream did not answer in time".to_string()
    } else if error.is_connect() {
        "the upstream could not be reached".to_string()
    } else if error.is_redirect() {
        "the upstream redirected the request".to_string()
    } else {
        "the upstream connection failed".to_string()
    }
}

/// Streams one response to the client, recording what it carried.
///
/// `adopt_pin` stores the Key this request used so the next turn of the same
/// conversation stays on it.
pub(crate) async fn forward(
    service: Arc<ModelGatewayService>,
    prepared: PreparedCall,
    attempt: UpstreamAttempt,
    key: &KeyReference,
    sender: mpsc::Sender<Result<bytes::Bytes, GatewayError>>,
) {
    let UpstreamAttempt {
        response,
        target,
        passthrough,
        profile_id,
        failed_attempts,
        skipped_members,
    } = attempt;
    // The pin follows the member that actually answered, not the first one the
    // plan preferred: a conversation that failed over must keep using the Key
    // that worked.
    if let (Some(conversation), Some(key)) = (prepared.conversation.as_deref(), Some(&key.id)) {
        service.pin_conversation(conversation, &profile_id, key);
    }
    let answered_model = prepared
        .member(&profile_id)
        .map(|member| member.model.clone())
        .unwrap_or_else(|| prepared.requested_model.clone());
    let started = std::time::Instant::now();
    let mut sniffer = UsageSniffer::new(target);
    let mut buffer = SseBuffer::default();
    let request_for_encoder = CanonicalRequest::for_model(answered_model.clone());
    let mut encoder: Option<Box<dyn StreamEncoder>> =
        (!passthrough).then(|| encoder(prepared.inbound, &request_for_encoder));
    let mut accumulator = ReplyAccumulator::default();
    let mut stream = response.bytes_stream();
    let mut failed = false;
    let mut last_output = std::time::Instant::now();
    // A vendor may accept the connection and then say nothing. No error arrives,
    // so the error-based failover never fires and the turn hangs. The first
    // token is the deadline this route set; afterwards a slow reply is the
    // model thinking, which is legitimate and must not be cut.
    let first_token_deadline = prepared
        .member(&profile_id)
        .and_then(|member| member.first_token_timeout_ms)
        .map(|timeout| started + std::time::Duration::from_millis(timeout));
    let mut first_token_seen = false;

    loop {
        let next = match first_token_deadline.filter(|_| !first_token_seen) {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match tokio::time::timeout(remaining, stream.next()).await {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        // Nothing streamed in time. The client has already
                        // received nothing, so this is an in-stream error
                        // rather than a silent stall.
                        failed = true;
                        break;
                    }
                }
            }
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let Ok(chunk) = chunk else {
            failed = true;
            break;
        };
        if !first_token_seen {
            first_token_seen = true;
            // The deadline only guards the wait for the first token; the event
            // is what lets a client stop drawing the member as "starting".
            service.note_first_token(&prepared.request_id, vibex_core::unix_timestamp_ms());
        }
        if passthrough {
            sniffer.observe(&buffer.push(&chunk));
            if sender.send(Ok(chunk)).await.is_err() {
                return;
            }
            continue;
        }
        let frames = buffer.push(&chunk);
        let Some(encoder) = encoder.as_mut() else {
            return;
        };
        let mut decoded = decoder(target);
        let mut translated = String::new();
        let mut decode_error = None;
        for (_, data) in frames {
            if data.trim() == "[DONE]" {
                continue;
            }
            match decoded.decode(&data) {
                Ok(events) => {
                    for event in events {
                        accumulator.apply(&event);
                        match encoder.encode(&event) {
                            Ok(bytes) => translated.push_str(&bytes),
                            Err(error) => decode_error = Some(error),
                        }
                    }
                }
                Err(error) => decode_error = Some(error),
            }
        }
        if !translated.is_empty() {
            last_output = std::time::Instant::now();
            if sender
                .send(Ok(bytes::Bytes::from(translated)))
                .await
                .is_err()
            {
                return;
            }
        } else if last_output.elapsed() >= std::time::Duration::from_secs(10)
            && let Some(keepalive) = encoder.keepalive()
        {
            // A long tool call can stall the upstream for a while. Protocols
            // that tolerate a comment get one so the Agent's read timeout does
            // not fire mid-reply.
            last_output = std::time::Instant::now();
            if sender
                .send(Ok(bytes::Bytes::from_static(keepalive.as_bytes())))
                .await
                .is_err()
            {
                return;
            }
        }
        if let Some(error) = decode_error {
            let frame = encoder
                .encode(&CanonicalEvent::Error {
                    status: 502,
                    message: error.message(),
                })
                .unwrap_or_default();
            let _ = sender.send(Ok(bytes::Bytes::from(frame))).await;
            failed = true;
            break;
        }
    }

    if !passthrough && let Some(encoder) = encoder.as_mut() {
        let mut decoded = decoder(target);
        for (_, data) in buffer.finish() {
            if data.trim() == "[DONE]" {
                continue;
            }
            for event in decoded.decode(&data).unwrap_or_default() {
                accumulator.apply(&event);
                if let Ok(bytes) = encoder.encode(&event) {
                    let _ = sender.send(Ok(bytes::Bytes::from(bytes))).await;
                }
            }
        }
        for event in decoded.finish().unwrap_or_default() {
            accumulator.apply(&event);
            if let Ok(bytes) = encoder.encode(&event) {
                let _ = sender.send(Ok(bytes::Bytes::from(bytes))).await;
            }
        }
        if let Ok(tail) = encoder.finish()
            && !tail.is_empty()
        {
            let _ = sender.send(Ok(bytes::Bytes::from(tail))).await;
        }
    }

    let usage = if passthrough {
        // A passthrough stream that never named a terminal event was cut. The
        // client already has the prefix, so the only honest answer is an
        // in-stream error after it.
        if !sniffer.saw_terminal() && !failed {
            failed = true;
        }
        sniffer.usage()
    } else {
        accumulator.usage
    };

    if failed {
        let frame = sse_frame(
            None,
            &serde_json::json!({
                "error": {
                    "type": "upstream_error",
                    "message": "the upstream stream ended before the reply was complete",
                }
            }),
        );
        let _ = sender.send(Ok(bytes::Bytes::from(frame))).await;
    }

    service.record(
        &prepared,
        &RequestOutcome {
            // A request only counts as a failover when a Key actually failed
            // before this one answered. Counting the size of the candidate pool
            // instead would label a first-try success as a failover on every
            // Provider that happens to hold more than one Key.
            kind: if failed {
                GatewayRequestOutcome::Failed
            } else if failed_attempts.is_empty() {
                GatewayRequestOutcome::Ok
            } else {
                GatewayRequestOutcome::Failover
            },
            credential: Some(key.id.clone()),
            status: Some(if failed { 502 } else { 200 }),
            profile_id: Some(profile_id.clone()),
            target: Some(target),
            skipped_cooling: prepared
                .member(&profile_id)
                .map(|member| member.skipped_cooling)
                .unwrap_or(0),
            skipped_over_limit: prepared
                .member(&profile_id)
                .map(|member| member.skipped_over_limit)
                .unwrap_or(0),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: None,
            failed_attempts,
            skipped_members,
        },
    );
}

/// Reads a whole upstream reply for a client that asked for one body.
pub(crate) async fn gather(
    service: &ModelGatewayService,
    prepared: &PreparedCall,
    attempt: UpstreamAttempt,
    key: &KeyReference,
) -> GatewayResult<Value> {
    let UpstreamAttempt {
        response,
        target,
        passthrough,
        profile_id,
        failed_attempts,
        skipped_members,
    } = attempt;
    let started = std::time::Instant::now();
    // A non-streaming client gets one body, so the whole reply is its "first
    // token": the deadline has already done its job by the time the body
    // arrives, and the graph still needs to stop drawing this request as
    // starting.
    service.note_first_token(&prepared.request_id, vibex_core::unix_timestamp_ms());
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => {
            // The reply body never arrived. The ledger keeps the fact, so a
            // request that produced neither an answer nor a token count is
            // still visible as a failure rather than as a gap.
            service.record(
                prepared,
                &RequestOutcome {
                    kind: GatewayRequestOutcome::Failed,
                    profile_id: Some(profile_id.clone()),
                    target: Some(target),
                    skipped_cooling: 0,
                    skipped_over_limit: 0,
                    credential: Some(key.id.clone()),
                    status: Some(502),
                    usage: CanonicalUsage::default(),
                    latency_ms: Some(started.elapsed().as_millis() as i64),
                    served: None,
                    failed_attempts,
                    skipped_members,
                },
            );
            return Err(GatewayError::Upstream {
                status: 502,
                message: "the upstream reply ended before it was complete".to_string(),
            });
        }
    };
    let (response, usage) = if passthrough {
        let parsed = parse_reply(target, &bytes).unwrap_or_default();
        let usage = parsed.usage.clone();
        (parsed, usage)
    } else {
        // A translated request asks the upstream for a stream, but an upstream
        // that only speaks whole bodies may ignore the flag. Accept both
        // shapes rather than turning a valid answer into an empty one.
        if bytes
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'{')
        {
            let parsed = parse_reply(target, &bytes).unwrap_or_default();
            let usage = parsed.usage.clone();
            (parsed, usage)
        } else {
            let mut sniffer = UsageSniffer::new(target);
            let mut buffer = SseBuffer::default();
            let mut accumulator = ReplyAccumulator::default();
            let mut decoded = decoder(target);
            for (_, data) in buffer.push(&bytes) {
                if data.trim() == "[DONE]" {
                    continue;
                }
                for event in decoded.decode(&data).unwrap_or_default() {
                    accumulator.apply(&event);
                }
            }
            sniffer.observe(&buffer.finish());
            if sniffer.usage().total() > 0 {
                accumulator.apply(&CanonicalEvent::Usage(sniffer.usage()));
            }
            let parsed = accumulator.finish();
            let usage = parsed.usage.clone();
            (parsed, usage)
        }
    };
    let rendered = render_response(prepared.inbound, &response)?;
    if let Some(conversation) = prepared.conversation.as_deref() {
        service.pin_conversation(conversation, &profile_id, &key.id);
    }
    service.record(
        prepared,
        &RequestOutcome {
            kind: if failed_attempts.is_empty() {
                GatewayRequestOutcome::Ok
            } else {
                GatewayRequestOutcome::Failover
            },
            credential: Some(key.id.clone()),
            status: Some(200),
            profile_id: Some(profile_id.clone()),
            target: Some(target),
            skipped_cooling: prepared
                .member(&profile_id)
                .map(|member| member.skipped_cooling)
                .unwrap_or(0),
            skipped_over_limit: prepared
                .member(&profile_id)
                .map(|member| member.skipped_over_limit)
                .unwrap_or(0),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: (!response.model.is_empty()).then(|| response.model.clone()),
            failed_attempts,
            skipped_members,
        },
    );
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(protocols: &[WireProtocol]) -> GatewayUpstream {
        GatewayUpstream {
            profile_id: ModelProviderProfileId::new(),
            display_name: "Upstream".to_string(),
            endpoints: protocols
                .iter()
                .map(|protocol| ModelProviderEndpoint {
                    id: format!("ep-{}", protocol.short_name()),
                    kind: ModelProviderEndpointKind::Api,
                    url: "https://example.invalid".to_string(),
                    wire_protocol_id: Some(protocol.wire_protocol_id().to_string()),
                })
                .collect(),
            models: vec!["m".to_string()],
            credentials: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// Three failures degrade, one success recovers.
    ///
    /// The transition is the whole contract: a degraded gateway must come back
    /// without a restart, and a single good answer is the signal.
    #[test]
    fn the_breaker_degrades_after_three_failures_and_recovers_on_success() {
        use std::sync::atomic::Ordering;
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-test.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-test-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure();
        }
        assert!(service.is_degraded());
        assert!(service.runtime_state().notice.is_some());
        // The listener is still bound, so the UI can distinguish "stepped
        // aside" from "not running".
        assert!(service.runtime_state().status.is_listening());

        service.note_success();
        assert!(!service.is_degraded());
        assert_eq!(service.runtime_state().status, ModelGatewayStatus::Running);
        assert!(service.runtime_state().notice.is_none());
        assert_eq!(service.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    /// A degraded gateway admits exactly one request at a time.
    ///
    /// This is what makes recovery possible at all: the single probe is the
    /// only request that can reach an upstream and produce the success
    /// `note_success` needs. A second caller is refused rather than queued, so
    /// it stays on its direct Provider instead of waiting behind a broken one.
    #[test]
    fn a_degraded_gateway_admits_one_probe_at_a_time() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-probe.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-probe-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure();
        }
        assert!(service.is_degraded());

        // While running, every request goes through and claims nothing.
        let running = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-running.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-running-db.db")),
            GatewayLimits::default(),
        );
        running.publish(ModelGatewayRuntimeState::running(3425, 1));
        assert!(!running.begin_attempt().unwrap());

        // The first caller through a degraded gateway holds the slot.
        assert!(service.begin_attempt().unwrap());
        // A second one is refused with the message the server must not count.
        let refused = service.begin_attempt().unwrap_err();
        assert!(
            matches!(&refused, GatewayError::NotConfigured(message)
                if message == ModelGatewayService::STEPPED_ASIDE),
            "the refusal must be recognisable as the gateway's own, got {refused:?}"
        );

        // Releasing lets the next request probe.
        service.finish_attempt(true);
        assert!(service.begin_attempt().unwrap());
        service.finish_attempt(true);
    }

    /// A probe that reaches an upstream and answers clears the state.
    #[test]
    fn a_successful_probe_leaves_the_degraded_state() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-heal.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-heal-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure();
        }
        assert!(service.is_degraded());

        assert!(service.begin_attempt().unwrap());
        service.note_success();

        assert!(!service.is_degraded());
        assert_eq!(service.runtime_state().status, ModelGatewayStatus::Running);
        // The next request is ordinary again, not a probe.
        assert!(!service.begin_attempt().unwrap());
    }

    /// A failure from an unrelated outage starts the count over.
    #[test]
    fn an_old_failure_does_not_accumulate() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-old.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-old-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        service.note_failure();
        service.note_failure();
        service.last_failure_at_ms.store(
            (vibex_core::unix_timestamp_ms() - ModelGatewayService::FAILURE_MEMORY_MS - 1) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        service.note_failure();
        assert!(
            !service.is_degraded(),
            "a failure from an unrelated outage must not complete the count"
        );
    }

    #[test]
    fn endpoint_join_does_not_double_the_version_prefix() {
        assert_eq!(
            endpoint_url(
                "https://api.deepseek.com/v1",
                WireProtocol::ChatCompletions,
                "deepseek-chat",
                false
            )
            .unwrap(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url(
                "https://api.deepseek.com",
                WireProtocol::AnthropicMessages,
                "deepseek-chat",
                false
            )
            .unwrap(),
            "https://api.deepseek.com/v1/messages"
        );
        assert_eq!(
            endpoint_url(
                "https://generativelanguage.googleapis.com",
                WireProtocol::GeminiGenerateContent,
                "gemini-2.5-pro",
                false
            )
            .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn passthrough_wins_over_translation() {
        let upstream = upstream(&[
            WireProtocol::ChatCompletions,
            WireProtocol::AnthropicMessages,
        ]);
        let policy = GatewayPolicy::default();
        assert_eq!(
            choose_target(WireProtocol::ChatCompletions, &upstream, &policy).unwrap(),
            (WireProtocol::ChatCompletions, true)
        );
        // A protocol the upstream does not speak is translated.
        assert_eq!(
            choose_target(WireProtocol::Responses, &upstream, &policy).unwrap(),
            (WireProtocol::AnthropicMessages, false)
        );
    }

    #[test]
    fn a_disabled_translation_is_refused_instead_of_silently_used() {
        let upstream = upstream(&[WireProtocol::ChatCompletions]);
        let mut policy = GatewayPolicy::default();
        policy.translate.openai_chat_completions = false;
        let error = choose_target(WireProtocol::Responses, &upstream, &policy).unwrap_err();
        assert_eq!(error.code(), "model_gateway_translation_disabled");
    }

    #[test]
    fn upstream_messages_are_extracted_and_truncated() {
        assert_eq!(
            upstream_message(r#"{"error":{"message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(upstream_message("plain failure"), "plain failure");
        let long = "x".repeat(600);
        let truncated = upstream_message(&long);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 513);
    }

    #[test]
    fn translation_policy_gates_only_the_protocols_it_names() {
        let mut policy = vibex_core::GatewayTranslatePolicy::default();
        assert!(policy.allows(WireProtocol::ChatCompletions.wire_protocol_id()));
        policy.openai_chat_completions = false;
        assert!(!policy.allows(WireProtocol::ChatCompletions.wire_protocol_id()));
        // A protocol the gateway cannot translate is not gated by this switch.
        assert!(policy.allows(vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE));
    }

    #[test]
    fn model_catalog_is_qualified_by_upstream() {
        let mut left = upstream(&[WireProtocol::ChatCompletions]);
        left.profile_id =
            ModelProviderProfileId::parse(format!("model_provider_{}", RequestId::new().as_str()))
                .unwrap();
        let mut right = left.clone();
        right.profile_id =
            ModelProviderProfileId::parse(format!("model_provider_{}", RequestId::new().as_str()))
                .unwrap();
        let snapshot = GatewaySnapshot {
            settings: ModelGatewaySettings::default(),
            upstreams: vec![left, right],
            gateway_used_tokens: 0,
            routes: BTreeMap::new(),
            groups: BTreeMap::new(),
        };
        let catalog = snapshot.model_catalog();
        assert_eq!(catalog.len(), 2);
        // Two upstreams may share a display name; their published ids must
        // still be distinct and resolvable.
        assert_ne!(catalog[0].id, catalog[1].id);
        for entry in &catalog {
            let namespaces = snapshot.model_namespaces();
            assert!(
                resolve_model(&entry.id, &namespaces).profile().is_some(),
                "published model id {} must resolve",
                entry.id
            );
        }
    }
}

#[cfg(test)]
mod inflight_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn service() -> Arc<ModelGatewayService> {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join(format!("vibex-inflight-{}.db", RequestId::new().as_str())),
            ProviderConfigService::new(std::env::temp_dir().join(format!(
                "vibex-inflight-cfg-{}.db",
                RequestId::new().as_str()
            ))),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        service
    }

    fn inflight(request_id: &RequestId, attempt: u32) -> vibex_core::GatewayInflight {
        vibex_core::GatewayInflight {
            request_id: request_id.clone(),
            agent_id: Some("codex".to_string()),
            model_route_id: Some("model_route_test".to_string()),
            requested_model: "route/model_route_test".to_string(),
            model_provider_profile_id: ModelProviderProfileId::new(),
            attempt,
            credential_id: Some(RequestId::new()),
            wire_protocol_id: WireProtocol::ChatCompletions.wire_protocol_id().to_string(),
            started_at_ms: vibex_core::unix_timestamp_ms(),
            first_token_at_ms: None,
        }
    }

    /// A request is visible while it runs, not only once it has finished.
    #[test]
    fn an_inflight_request_is_listed_until_it_ends() {
        let service = service();
        let request_id = RequestId::new();
        assert!(service.inflight_requests().is_empty());

        service.note_inflight(inflight(&request_id, 1));
        let listed = service.inflight_requests();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].request_id, request_id);
        assert!(!listed[0].has_first_token());

        service.note_first_token(&request_id, vibex_core::unix_timestamp_ms());
        assert!(
            service.inflight_requests()[0].has_first_token(),
            "the first token ends the deadline and the starting state"
        );

        service.note_finished(&request_id, vibex_core::GatewayRequestOutcome::Ok);
        assert!(
            service.inflight_requests().is_empty(),
            "a finished request must not stay drawn"
        );
    }

    /// Moving to another member replaces the entry rather than adding one.
    #[test]
    fn a_member_retry_updates_the_same_request() {
        let service = service();
        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_inflight(inflight(&request_id, 2));

        let listed = service.inflight_requests();
        assert_eq!(listed.len(), 1, "one request, not one per attempt");
        assert_eq!(listed[0].attempt, 2, "the attempt number advances");
    }

    /// The table is bounded, so a long-running gateway cannot grow it forever.
    #[test]
    fn the_inflight_table_keeps_only_the_newest_requests() {
        let service = service();
        for _ in 0..ModelGatewayService::MAX_INFLIGHT + 3 {
            service.note_inflight(inflight(&RequestId::new(), 1));
        }
        assert_eq!(
            service.inflight_requests().len(),
            ModelGatewayService::MAX_INFLIGHT
        );
    }

    /// Every transition reaches the sink, in order.
    #[test]
    fn each_transition_is_published_to_the_sink() {
        let service = service();
        let seen: Arc<StdMutex<Vec<vibex_core::GatewayEvent>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        service.set_event_sink(Arc::new(move |event| {
            sink.lock().unwrap().push(event);
        }));

        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_first_token(&request_id, 42);
        service.note_finished(&request_id, vibex_core::GatewayRequestOutcome::Failover);

        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], vibex_core::GatewayEvent::Began(_)));
        assert!(matches!(
            events[1],
            vibex_core::GatewayEvent::FirstToken { at_ms: 42, .. }
        ));
        assert!(matches!(
            events[2],
            vibex_core::GatewayEvent::Ended {
                outcome: vibex_core::GatewayRequestOutcome::Failover,
                ..
            }
        ));
        // Every event names the same request, which is what lets a client
        // follow one request rather than guess.
        assert!(events.iter().all(|event| event.request_id() == &request_id));
    }

    /// A first token is only announced once.
    #[test]
    fn the_first_token_is_announced_once() {
        let service = service();
        let seen: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let sink = Arc::clone(&seen);
        service.set_event_sink(Arc::new(move |event| {
            if matches!(event, vibex_core::GatewayEvent::FirstToken { .. }) {
                *sink.lock().unwrap() += 1;
            }
        }));
        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_first_token(&request_id, 1);
        service.note_first_token(&request_id, 2);
        assert_eq!(*seen.lock().unwrap(), 1);
    }
}
