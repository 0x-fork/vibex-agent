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
    GatewayConversationPin, GatewayFailureKind, GatewayFailureKind as FailureKind, GatewayPinScope,
    GatewayPolicy, GatewayRequestOutcome, GatewayTokenWindow, GatewayTokenWindowUsage,
    ModelGatewayRequestLog, ModelGatewayRuntimeState, ModelGatewaySettings, ModelGatewayStatus,
    ModelProviderAccountState, ModelProviderCredentialReference, ModelProviderEndpoint,
    ModelProviderEndpointKind, ModelProviderProfileId, ModelProviderProfileStatus, RequestId,
};
use vibex_db::{
    DbConnection, GatewayConversationPinRepository, GatewayTokenWindowRepository,
    ModelGatewayRequestLogRepository, ModelGatewaySettingsRepository,
    ModelProviderAccountStateRepository, apply_migrations, open_database,
};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalUsage};
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
}

impl GatewaySnapshot {
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

    /// Every model the gateway can serve, as `prefix/model` ids.
    pub fn model_catalog(&self) -> Vec<GatewayModelEntry> {
        let namespaces = self.model_namespaces();
        let mut entries = Vec::new();
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
    listener: Mutex<Option<ListenerHandle>>,
    snapshot: RwLock<Option<(GatewaySnapshot, std::time::Instant)>>,
    /// Set while the service is shutting down, so an in-flight request stops
    /// instead of starting a new upstream call.
    shutting_down: RwLock<bool>,
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
            listener: Mutex::new(None),
            snapshot: RwLock::new(None),
            shutting_down: RwLock::new(false),
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
        let mut gateway_used_tokens = 0;
        let mut upstreams = Vec::with_capacity(settings.upstreams.len());
        for profile_id in &settings.upstreams {
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
        Ok(GatewaySnapshot {
            settings,
            upstreams,
            gateway_used_tokens,
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

    /// Starts the listener, persisting the port that actually bound.
    ///
    /// A port collision probes upward from the persisted port and stores the
    /// result, so the next start does not move again and Agents keep the same
    /// base URL.
    pub async fn start(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
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

        let listener = self.bind(settings.port).await?;
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

    fn publish(&self, state: ModelGatewayRuntimeState) {
        if let Ok(mut guard) = self.runtime.write() {
            *guard = state;
        }
    }

    /// Stops the listener. Idempotent.
    pub async fn shutdown(&self) {
        if let Ok(mut shutting_down) = self.shutting_down.write() {
            *shutting_down = true;
        }
        let handle = self.listener.lock().ok().and_then(|mut guard| guard.take());
        if let Some(handle) = handle {
            handle.task.abort();
            let _ = handle.task.await;
        }
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
        if let Some(limit) = snapshot.settings.policy.token_limit
            && snapshot.gateway_used_tokens >= limit.tokens
        {
            return Err(GatewayError::TokenCeilingReached {
                provider: snapshot.settings.display_name.clone(),
            });
        }

        let namespaces = snapshot.model_namespaces();
        let resolved = resolve_model(&request.model, &namespaces);
        let (profile_id, requested_model) = match resolved {
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
            ResolvedModel::NotFound { requested, closest } => {
                return Err(GatewayError::ModelNotFound {
                    model: requested,
                    closest,
                });
            }
        };
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
        let now = vibex_core::unix_timestamp_ms();
        let connection = self.connection()?;
        let states = ModelProviderAccountStateRepository::list(&connection, &profile_id)?;
        let state_by_key = states
            .into_iter()
            .map(|state| (state.credential_id.clone(), state))
            .collect::<BTreeMap<_, _>>();
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
        let conversation = (policy.pin == GatewayPinScope::Conversation)
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

        let mut candidates = Vec::new();
        for (index, credential) in upstream.credentials.iter().enumerate() {
            let state = state_by_key
                .get(&credential.id)
                .cloned()
                .unwrap_or_else(|| {
                    ModelProviderAccountState::ready(profile_id.clone(), credential.id.clone())
                });
            let mut candidate =
                CandidateKey::new(profile_id.clone(), credential.id.clone(), index as i64);
            if let Some(limit) = policy.token_limit {
                candidate = candidate.with_token_limit(limit, snapshot.gateway_used_tokens);
            }
            candidate = match state.visibility_at(now) {
                vibex_core::GatewayKeyState::Ready => candidate,
                vibex_core::GatewayKeyState::Cooling { kind, .. } => candidate.cooling(kind),
                vibex_core::GatewayKeyState::Invalid => candidate.cooling(FailureKind::Auth),
                vibex_core::GatewayKeyState::Disabled => candidate.cooling(FailureKind::Other),
            };
            candidates.push((candidate, credential.clone()));
        }
        if candidates.is_empty() {
            return Err(GatewayError::NotConfigured(format!(
                "`{}` has no API Key",
                upstream.display_name
            )));
        }

        let plan = plan_route(RouteRequest {
            candidates: candidates
                .iter()
                .map(|(candidate, _)| candidate.clone())
                .collect(),
            strategy: policy.routing,
            rotation: self.rotation.fetch_add(1, Ordering::Relaxed),
            pinned: pinned.clone(),
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

        let body = build_upstream_request(target, &request, &requested_model)?;
        Ok(PreparedCall {
            inbound,
            profile_id,
            provider_name: upstream.display_name.clone(),
            requested_model: request.model.clone(),
            model: requested_model,
            target,
            passthrough,
            endpoint,
            body,
            keys,
            agent_id,
            conversation,
            stream: request.stream,
        })
    }

    /// Sends one attempt, trying the prepared candidates in order until one
    /// answers with a success status. A failure before any byte reaches the
    /// client may fail over; the caller decides whether that is still allowed.
    pub(crate) async fn open_attempt(
        &self,
        prepared: &PreparedCall,
    ) -> GatewayResult<(UpstreamAttempt, KeyReference, Vec<String>)> {
        if self.shutting_down.read().map(|flag| *flag).unwrap_or(false) {
            return Err(GatewayError::NotConfigured(
                "the gateway is shutting down".to_string(),
            ));
        }
        let mut last_error: Option<GatewayError> = None;
        let mut skipped = Vec::new();
        for (index, (candidate, credential)) in prepared.keys.iter().enumerate() {
            if index > 0 {
                skipped.push(format!(
                    "{} failed before the reply began",
                    candidate.credential.as_str()
                ));
            }
            let secret = match resolve_key_secret(credential) {
                Ok(Some(secret)) => secret,
                Ok(None) => {
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key",
                        prepared.provider_name
                    )));
                    continue;
                }
                Err(error) => {
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key: {}",
                        prepared.provider_name, error.code
                    )));
                    continue;
                }
            };
            let url = endpoint_url(
                &prepared.endpoint.url,
                prepared.target,
                &prepared.model,
                prepared.stream,
            )?;
            let mut wire = apply_auth(
                self.client.post(&url).json(&prepared.body),
                prepared.target,
                &secret,
            );
            if let Some(beta) = beta_header(&prepared.target, &prepared.body) {
                wire = wire.header("anthropic-beta", beta);
            }
            let response = match wire.send().await {
                Ok(response) => response,
                Err(error) => {
                    let kind = if error.is_timeout() {
                        FailureKind::Upstream
                    } else {
                        FailureKind::Proxy
                    };
                    self.mark_failure(&prepared.profile_id, &candidate.credential, kind);
                    last_error = Some(GatewayError::Upstream {
                        status: 502,
                        message: format!(
                            "{}: {}",
                            prepared.provider_name,
                            redact_transport(&error)
                        ),
                    });
                    continue;
                }
            };
            if response.status().is_success() {
                self.clear_failure(&prepared.profile_id, &candidate.credential);
                return Ok((
                    UpstreamAttempt {
                        response,
                        target: prepared.target,
                        passthrough: prepared.passthrough,
                    },
                    KeyReference {
                        id: candidate.credential.clone(),
                    },
                    skipped,
                ));
            }
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            let kind = classify_failure(status, &body);
            self.mark_failure(&prepared.profile_id, &candidate.credential, kind);
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
                state.cooldown_reason = Some(FailureKind::Auth);
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
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let log = ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: prepared.agent_id.clone(),
            model_provider_profile_id: prepared.profile_id.clone(),
            credential_id: outcome.credential.clone(),
            model_requested: prepared.requested_model.clone(),
            model_served: outcome.served.clone(),
            wire_in: prepared.inbound.wire_protocol_id().to_string(),
            wire_out: prepared.target.wire_protocol_id().to_string(),
            status: outcome.status,
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cached_read_tokens: outcome.usage.cached_read_tokens,
            cached_write_tokens: outcome.usage.cached_write_tokens,
            reasoning_tokens: outcome.usage.reasoning_tokens,
            latency_ms: outcome.latency_ms,
            outcome: outcome.kind,
            created_at_ms: now,
        };
        let _ = ModelGatewayRequestLogRepository::insert(&connection, &log);
        if outcome.usage.total() > 0
            && let Ok(settings) = ModelGatewaySettingsRepository::get_or_create(&connection)
            && let Some(limit) = settings.policy.token_limit
        {
            let _ = GatewayTokenWindowRepository::record(
                &connection,
                &prepared.profile_id,
                limit.window,
                outcome.usage.total(),
                now,
            );
        }
    }

    /// Answers one call end to end, gathering a stream when the client asked
    /// for a single body.
    pub async fn call(
        &self,
        inbound: WireProtocol,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> GatewayResult<PreparedCall> {
        let _ = headers;
        let _ = body;
        Err(GatewayError::Internal(format!(
            "the {inbound:?} endpoint requires the streaming executor"
        )))
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

/// A request that has been resolved but not yet sent.
#[derive(Debug, Clone)]
pub struct PreparedCall {
    pub inbound: WireProtocol,
    pub profile_id: ModelProviderProfileId,
    pub provider_name: String,
    pub requested_model: String,
    pub model: String,
    pub target: WireProtocol,
    pub passthrough: bool,
    pub endpoint: ModelProviderEndpoint,
    pub body: Value,
    /// Ordered attempts: the candidate and the credential record it uses.
    pub keys: Vec<(CandidateKey, ModelProviderCredentialReference)>,
    pub agent_id: Option<String>,
    pub conversation: Option<String>,
    pub stream: bool,
}

/// What one finished request produced, as the ledger records it.
pub(crate) struct RequestOutcome {
    pub kind: GatewayRequestOutcome,
    pub credential: Option<RequestId>,
    pub status: Option<u16>,
    pub usage: CanonicalUsage,
    pub latency_ms: Option<i64>,
    /// The model the upstream said it served, when it said one.
    pub served: Option<String>,
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
    } = attempt;
    if let (Some(conversation), Some((_, credential))) =
        (prepared.conversation.as_deref(), prepared.keys.first())
    {
        service.pin_conversation(conversation, &prepared.profile_id, &credential.id);
    }
    let started = std::time::Instant::now();
    let mut sniffer = UsageSniffer::new(target);
    let mut buffer = SseBuffer::default();
    let request_for_encoder = CanonicalRequest::for_model(prepared.model.clone());
    let mut encoder: Option<Box<dyn StreamEncoder>> =
        (!passthrough).then(|| encoder(prepared.inbound, &request_for_encoder));
    let mut accumulator = ReplyAccumulator::default();
    let mut stream = response.bytes_stream();
    let mut failed = false;
    let mut last_output = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            failed = true;
            break;
        };
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
            kind: if failed {
                GatewayRequestOutcome::Failed
            } else if prepared.keys.len() > 1 {
                GatewayRequestOutcome::Failover
            } else {
                GatewayRequestOutcome::Ok
            },
            credential: Some(key.id.clone()),
            status: Some(if failed { 502 } else { 200 }),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: None,
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
    } = attempt;
    let started = std::time::Instant::now();
    let bytes = response.bytes().await.map_err(|_| GatewayError::Upstream {
        status: 502,
        message: "the upstream reply ended before it was complete".to_string(),
    })?;
    let (response, usage) = if passthrough {
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
    };
    let rendered = render_response(prepared.inbound, &response)?;
    if let Some(conversation) = prepared.conversation.as_deref() {
        service.pin_conversation(conversation, &prepared.profile_id, &key.id);
    }
    service.record(
        prepared,
        &RequestOutcome {
            kind: GatewayRequestOutcome::Ok,
            credential: Some(key.id.clone()),
            status: Some(200),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: (!response.model.is_empty()).then(|| response.model.clone()),
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
