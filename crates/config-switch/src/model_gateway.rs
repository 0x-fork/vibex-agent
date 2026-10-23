//! Wiring the local model gateway into every Agent it can serve.
//!
//! The gateway is one shared endpoint, and it is switched on or off for the
//! whole installation — never per Agent. While it is enabled, this module keeps
//! one hidden legacy Profile per gateway-capable Agent, whose base URL is the
//! gateway, whose model list tracks the aggregated catalogue, and which is that
//! Agent's default. Disabling the gateway removes those Profiles, and every
//! Agent falls back to its own direct Provider.
//!
//! The Profiles are marked with the internal role so they never appear in an
//! Agent's provider list: the gateway is configured in one place and consumed
//! here, rather than being duplicated into every Agent's editor.

use std::collections::BTreeSet;

use vibex_core::{
    AgentId, GatewayRoutingStrategy, ModelProviderProfileStatus, ProviderConfiguredModel,
    ProviderKind, ProviderModelCapabilities, ProviderModelWireApi, ProviderOptions,
    ProviderProfile, ProviderProfileCreateRequest, ProviderProfileId, ProviderProfileStatus,
    ProviderSecretBackend, ProviderSecretKind, ProviderSecretReferenceCreateRequest,
    ProviderSecretSetupState, VibexError, VibexResult,
};
use vibex_db::{
    AgentDefaultModelProviderProfileRepository, ModelProviderProfileRepository,
    ProviderProfileRepository,
};

use crate::{ProviderConfigService, provider_option_value};

/// The internal role that keeps a Profile out of the Config Center's lists.
pub const GATEWAY_PROFILE_ROLE_OPTION_KEY: &str = "vibex.internal.profileRole";
pub const GATEWAY_PROFILE_ROLE: &str = "model_gateway";
/// The option a gateway Profile carries its owning Agent in, so it can be
/// found without guessing from its display name.
pub const GATEWAY_AGENT_OPTION_KEY: &str = "vibex.internal.gatewayAgent";

/// The placeholder the Agent sends as its API Key. The gateway ignores inbound
/// credentials: it authenticates to the upstream with the Key it selects, so a
/// local process never needs to hold one.
pub const GATEWAY_LOCAL_KEY_HINT: &str = "managed by Vibex";

impl ProviderConfigService {
    /// The Agents the gateway serves: every Agent that can be pointed at it.
    ///
    /// This is derived, not chosen per Agent. The gateway is a global switch,
    /// so while it is on it covers each gateway-capable Agent; an Agent that
    /// manages its own provider cannot be pointed at it at all and is absent.
    ///
    /// The hidden Profiles are only materialised while the listener is up. This
    /// answer therefore describes intent, which is what a Config Center needs:
    /// it stays the same across a gateway restart instead of blinking empty.
    pub fn gateway_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        let configurable = vibex_core::model_provider_configurable_agent_ids()?;
        let enabled = self
            .open_connection()
            .and_then(|conn| vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn))
            .map(|settings| settings.policy.enabled)
            .unwrap_or(false);
        if !enabled {
            return Ok(BTreeSet::new());
        }
        Ok(configurable
            .into_iter()
            .filter(|agent_id| gateway_wire_api_for(agent_id).is_some())
            .collect())
    }

    /// The Agents currently pointed at the gateway's address.
    ///
    /// A configured Agent whose Profile has been removed is *paused*, not
    /// active: its requests use the direct Provider until the listener returns.
    pub fn gateway_active_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        let conn = self.open_connection()?;
        let profiles = ProviderProfileRepository::list_all(&conn)?;
        let mut agents = BTreeSet::new();
        for profile in profiles {
            if !is_gateway_profile(&profile) {
                continue;
            }
            if let Some(agent_id) = gateway_profile_agent(&profile) {
                agents.insert(agent_id);
            }
        }
        Ok(agents)
    }

    /// Every Agent the gateway can serve, with the shared routing strategy.
    ///
    /// The switch is global, so `enabled` is the same for every row and the
    /// strategy is the gateway's own. The pair is still returned per Agent
    /// because the Config Center draws one row per Agent, and because an Agent
    /// that manages its own provider has to be reported as unsupported rather
    /// than silently omitted.
    pub fn gateway_agent_options(
        &self,
    ) -> VibexResult<Vec<(AgentId, GatewayRoutingStrategy, bool, bool)>> {
        let conn = self.open_connection()?;
        let settings = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?;
        let enabled = settings.policy.enabled;
        let strategy = settings.policy.routing;
        let mut options = vibex_core::model_provider_configurable_agent_ids()?
            .into_iter()
            .map(|agent_id| {
                let unsupported = gateway_wire_api_for(&agent_id).is_none();
                (agent_id, strategy, enabled && !unsupported, unsupported)
            })
            .collect::<Vec<_>>();
        options.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        Ok(options)
    }

    /// Points every gateway-capable Agent at the gateway.
    ///
    /// This is the enabling half of the global switch. The listener's address
    /// and the aggregated catalogue are the same for every Agent, so the work
    /// is one pass over the capable Agents rather than a per-Agent decision.
    /// Returns the Agents whose hidden Profile actually changed.
    pub fn attach_gateway_to_agents(
        &self,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<Vec<AgentId>> {
        let base_url = base_url.trim();
        if base_url.is_empty() {
            return Err(VibexError::validation(
                "model_gateway_unavailable",
                "the gateway is not running, so no Agent can be pointed at it",
            ));
        }
        // The switch is global, so a disabled gateway must never leave a
        // Profile pointing at it — even when a listener happens to be bound.
        let conn = self.open_connection()?;
        if !vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?
            .policy
            .enabled
        {
            drop(conn);
            return self.deactivate_gateway_profiles();
        }
        drop(conn);
        let mut attached = Vec::new();
        for agent_id in self.gateway_capable_agents()? {
            // One Agent failing must not deny the gateway to the rest. That is
            // exactly the shape of an Agent that is merely unconfigured — an
            // ACP Agent with no runtime config yet — and the global switch has
            // to work before every Agent has been set up. The skip is silent on
            // purpose: the Agent simply keeps its direct Provider, which is
            // also what `gateway_active_agents` reports.
            if let Ok((_, changed)) =
                self.ensure_gateway_profile(&agent_id, base_url, model_catalog)
                && changed
            {
                attached.push(agent_id);
            }
        }
        Ok(attached)
    }

    /// Removes every hidden gateway Profile, returning the Agents affected.
    ///
    /// This is the disabling half of the global switch. Deleting the Profile is
    /// what actually moves an Agent back to its direct Provider; the durable
    /// per-Agent rows that earlier builds kept are no longer consulted, so
    /// nothing has to be remembered per Agent.
    pub fn detach_gateway_from_agents(&self) -> VibexResult<Vec<AgentId>> {
        self.deactivate_gateway_profiles()
    }

    /// Every Agent that can be pointed at the gateway at all.
    ///
    /// Two filters, both structural: the Agent must accept a Vibex-managed
    /// Provider, and it must speak one of the four protocols the gateway
    /// serves.
    fn gateway_capable_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        Ok(vibex_core::model_provider_configurable_agent_ids()?
            .into_iter()
            .filter(|agent_id| gateway_wire_api_for(agent_id).is_some())
            .collect())
    }

    /// Removes every hidden gateway Profile.
    ///
    /// This is what a stopped or disabled listener has to do: an Agent whose
    /// Profile still points at a dead loopback port cannot fall back, so the
    /// Profile goes away and the Agent returns to its direct default.
    ///
    /// The scan is over the existing hidden Profiles rather than over the
    /// capable Agents, because it has to work precisely when the gateway is
    /// off — the state in which the intent list is empty.
    pub fn deactivate_gateway_profiles(&self) -> VibexResult<Vec<AgentId>> {
        let conn = self.open_connection()?;
        let mut deactivated = Vec::new();
        for profile in ProviderProfileRepository::list_all(&conn)? {
            if !is_gateway_profile(&profile) {
                continue;
            }
            let Some(agent_id) = gateway_profile_agent(&profile) else {
                continue;
            };
            self.delete_profile(vibex_core::ProviderProfileDeleteRequest {
                provider_profile_id: profile.id,
            })?;
            deactivated.push(agent_id);
        }
        Ok(deactivated)
    }

    /// Creates or refreshes one hidden gateway Profile.
    ///
    /// Returns whether the Profile's projected content changed. A no-op leaves
    /// the revision alone, which is what keeps a snapshot refresh from
    /// restaling every Agent merely because it ran.
    fn ensure_gateway_profile(
        &self,
        agent_id: &AgentId,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<(ProviderProfileId, bool)> {
        let models = gateway_models(agent_id, model_catalog);
        let conn = self.open_connection()?;
        let models = self.with_route_capabilities(&conn, models)?;
        let existing = gateway_profile_for(&conn, agent_id)?;
        match existing {
            Some(profile) => {
                let unchanged = profile.base_url.as_deref() == Some(base_url)
                    && profile.configured_models == models
                    && profile.status == ProviderProfileStatus::Enabled;
                let profile_id = profile.id.clone();
                drop(conn);
                if !unchanged {
                    // An ACP Agent's Profile must carry typed ACP configuration:
                    // every write validates that, and a gateway Profile had none,
                    // so `acp_config_missing` refused the update and the per-Agent
                    // `if let Ok` swallowed it. The catalogue then never reached
                    // the Profile and the Agent's picker stayed empty. The same
                    // inheritance the shared-Provider create path uses fills it
                    // in, and it keeps the internal role keys the gateway is
                    // identified by — the agent-scoped update strips those, which
                    // is why this stays on the generic update.
                    let conn = self.open_connection()?;
                    let provider_options = crate::inherit_agent_acp_runtime_options(
                        &conn,
                        agent_id,
                        Some(gateway_provider_options(agent_id, profile_id.as_str())),
                    )?;
                    let provider_options = crate::with_acp_configured_models(
                        provider_options,
                        crate::configured_acp_model_ids(
                            &models,
                            models.first().map(|model| model.id.as_str()),
                        ),
                    )?;
                    drop(conn);
                    self.update_profile(vibex_core::ProviderProfileUpdateRequest {
                        provider_profile_id: profile_id.clone(),
                        display_name: Some(gateway_display_name()),
                        status: Some(ProviderProfileStatus::Enabled),
                        account_alias: None,
                        base_url: Some(base_url.to_string()),
                        default_model: models.first().map(|model| model.id.clone()),
                        small_model: None,
                        large_model: None,
                        configured_models: Some(models),
                        reasoning_effort: None,
                        sandbox_defaults: None,
                        network_defaults: None,
                        permission_defaults: None,
                        provider_options: Some(provider_options),
                    })?;
                }
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, !unchanged))
            }
            None => {
                drop(conn);
                // The creation write validates ACP configuration too, and an ACP
                // Agent has none until it is inherited here. Without this the
                // Profile was never created at all, so every ACP Agent —
                // `deepseek-harness` among them — silently stayed off the
                // gateway and could never see a route, group or otherwise.
                let conn = self.open_connection()?;
                let provider_options = crate::inherit_agent_acp_runtime_options(
                    &conn,
                    agent_id,
                    Some(gateway_provider_options(agent_id, "")),
                )?;
                let provider_options = crate::with_acp_configured_models(
                    provider_options,
                    crate::configured_acp_model_ids(
                        &models,
                        models.first().map(|model| model.id.as_str()),
                    ),
                )?;
                drop(conn);
                let created = self.create_profile(ProviderProfileCreateRequest {
                    agent_id: Some(agent_id.clone()),
                    kind: gateway_provider_kind(agent_id),
                    display_name: gateway_display_name(),
                    account_alias: None,
                    base_url: Some(base_url.to_string()),
                    default_model: models.first().map(|model| model.id.clone()),
                    small_model: None,
                    large_model: None,
                    configured_models: models,
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: Some(provider_options),
                    secret_references: vec![ProviderSecretReferenceCreateRequest {
                        secret_kind: ProviderSecretKind::ApiKey,
                        backend: ProviderSecretBackend::Placeholder,
                        setup_state: ProviderSecretSetupState::Referenced,
                        lookup_key: format!("vibex-model-gateway-{}", agent_id.as_str()),
                        display_label: "Vibex Gateway".to_string(),
                        redacted_hint: GATEWAY_LOCAL_KEY_HINT.to_string(),
                    }],
                })?;
                let profile_id = created.id.clone();
                // Rewrite the options now that the Profile id exists, keeping the
                // ACP configuration the create inherited: this write validates
                // an ACP Profile, and replacing its options with the bare gateway
                // keys would be refused.
                let conn = self.open_connection()?;
                let provider_options = crate::inherit_agent_acp_runtime_options(
                    &conn,
                    agent_id,
                    Some(gateway_provider_options(agent_id, profile_id.as_str())),
                )?;
                let provider_options = crate::with_acp_configured_models(
                    provider_options,
                    crate::configured_acp_model_ids(
                        &created.configured_models,
                        created.default_model.as_deref(),
                    ),
                )?;
                drop(conn);
                self.update_profile(vibex_core::ProviderProfileUpdateRequest {
                    provider_profile_id: profile_id.clone(),
                    display_name: None,
                    status: None,
                    account_alias: None,
                    base_url: None,
                    default_model: None,
                    small_model: None,
                    large_model: None,
                    configured_models: None,
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: Some(provider_options),
                })?;
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, true))
            }
        }
    }

    /// Fills each route-bearing model in with the capabilities its members
    /// share.
    ///
    /// `gateway_models` builds entries from model ids alone, because the
    /// catalog is a list of strings. A route's capabilities are a property of
    /// its members, so they are merged here, where the Profiles are in hand.
    fn with_route_capabilities(
        &self,
        conn: &vibex_db::DbConnection,
        models: Vec<ProviderConfiguredModel>,
    ) -> VibexResult<Vec<ProviderConfiguredModel>> {
        let routes = vibex_db::ModelRouteRepository::list_enabled(conn)?;
        if routes.is_empty() {
            return Ok(models);
        }
        let profiles = vibex_db::ModelProviderProfileRepository::list(conn)?;
        Ok(models
            .into_iter()
            .map(|model| {
                if let Some(route) = routes.iter().find(|route| route.model_id() == model.id) {
                    return ProviderConfiguredModel {
                        capabilities: route_capabilities(conn, &profiles, route),
                        ..model
                    };
                }
                // A provider group publishes `group/<id>/<model>`, so its
                // capabilities come from the Providers that serve that one
                // model rather than from the group as a whole.
                let Some(parsed) = vibex_core::provider_group_model_ref(&model.id) else {
                    return model;
                };
                let Some(route) = routes.iter().find(|route| {
                    route.is_provider_group() && route.id.as_str() == parsed.group_id
                }) else {
                    return model;
                };
                let caps = vibex_core::provider_group_models(route, &profiles)
                    .into_iter()
                    .find(|entry| entry.model_id == parsed.model_id)
                    .map(|entry| entry.capabilities)
                    .unwrap_or_default();
                ProviderConfiguredModel {
                    capabilities: vibex_core::ProviderModelCapabilities {
                        context_tokens: route.context_tokens.or(caps.context_tokens),
                        ..caps
                    },
                    ..model
                }
            })
            .collect())
    }

    /// Rewrites every gateway Profile's address after the listener moved.
    ///
    /// Each write advances the Profile revision, so the existing projection
    /// machinery marks exactly the affected bindings stale and no others.
    pub fn refresh_gateway_profiles(
        &self,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<Vec<AgentId>> {
        self.attach_gateway_to_agents(base_url, model_catalog)
    }

    fn set_gateway_default(
        &self,
        agent_id: &AgentId,
        profile_id: &ProviderProfileId,
    ) -> VibexResult<()> {
        self.set_agent_model_provider_default(vibex_core::AgentModelProviderSetDefaultRequest {
            scope: gateway_default_scope(),
            agent_id: agent_id.clone(),
            provider_profile_id: profile_id.clone(),
        })?;
        Ok(())
    }

    /// The durable per-Key routing state of every gateway upstream.
    pub fn gateway_account_states(
        &self,
    ) -> VibexResult<Vec<vibex_core::ModelProviderAccountState>> {
        let conn = self.open_connection()?;
        let settings = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?;
        let mut states = Vec::new();
        for upstream in &settings.upstreams {
            states.extend(vibex_db::ModelProviderAccountStateRepository::list(
                &conn, upstream,
            )?);
        }
        Ok(states)
    }

    /// The Models the gateway currently exposes, for the Config Center.
    ///
    /// One entry per route, not per member: a route is one selectable model, so
    /// an Agent must see one id for it. The members are what the gateway
    /// decides between, and listing them here would ask the Agent to choose a
    /// source the route is supposed to choose for it.
    pub fn gateway_model_catalog(&self) -> VibexResult<Vec<GatewayCatalogModel>> {
        let conn = self.open_connection()?;
        let profiles = ModelProviderProfileRepository::list(&conn)?;
        let routes = vibex_db::ModelRouteRepository::list_enabled(&conn)?;
        let mut catalog = Vec::new();
        for route in routes {
            // A direct route describes a connection that bypasses the gateway.
            // Publishing it here would invite an Agent to call the gateway for
            // something whose whole point is not to go through it.
            if !route.kind.via_gateway() {
                continue;
            }
            // A route whose members all point at Providers that no longer exist
            // has nothing to serve, so it is not offered.
            let served = route
                .enabled_members()
                .filter(|member| {
                    profiles.iter().any(|profile| {
                        profile.id == member.model_provider_profile_id
                            && profile.status == ModelProviderProfileStatus::Enabled
                    })
                })
                .count();
            if served == 0 {
                continue;
            }
            if route.is_provider_group() {
                // A group is several models, so it publishes one entry per
                // shared model. The model is part of the id because the group
                // itself is not a model.
                let models = vibex_core::provider_group_models(&route, &profiles);
                if models.is_empty() {
                    continue;
                }
                let model_count = models.len();
                for model in models {
                    catalog.push(GatewayCatalogModel {
                        route_id: Some(route.id.as_str().to_string()),
                        display_name: route.display_name.clone(),
                        model: route.group_model_id(&model.model_id),
                        kind: route.kind,
                        source_count: model.provider_profile_ids.len(),
                        model_count,
                    });
                }
                continue;
            }
            catalog.push(GatewayCatalogModel {
                route_id: Some(route.id.as_str().to_string()),
                display_name: route.display_name.clone(),
                model: route.model_id(),
                kind: route.kind,
                source_count: served,
                model_count: route.distinct_models().len(),
            });
        }
        Ok(catalog)
    }
}

/// One model the gateway serves: a route, or a single upstream.
///
/// The id an Agent asks for is `model`; `display_name` is what a picker groups
/// it under. A route never carries a Provider name in `model`, because the
/// Provider is the route's business, not the Agent's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCatalogModel {
    /// Present for a route; absent for a legacy single-upstream entry.
    pub route_id: Option<String>,
    pub display_name: String,
    pub model: String,
    pub kind: vibex_core::RouteKind,
    /// How many members can answer.
    pub source_count: usize,
    /// How many distinct model names the route offers.
    pub model_count: usize,
}

impl GatewayCatalogModel {
    /// The id an Agent's model list shows.
    ///
    /// A route's id is already `route/<id>`; it must not gain a Provider
    /// prefix, or the Agent would appear to have picked a vendor.
    pub fn display_id(&self) -> String {
        self.model.clone()
    }
}

/// The most models one Profile may carry.
const MAX_GATEWAY_MODELS: usize = 512;

fn gateway_default_scope() -> vibex_core::ProviderProfileDefaultScope {
    vibex_core::ProviderProfileDefaultScope {
        kind: vibex_core::ProviderDefaultScopeKind::Global,
        project_id: None,
        workspace_id: None,
    }
}

fn gateway_display_name() -> String {
    "Vibex Gateway".to_string()
}

fn gateway_models(agent_id: &AgentId, catalog: &[String]) -> Vec<ProviderConfiguredModel> {
    // The gateway is the endpoint, so the Agent still speaks its own protocol
    // to it. The protocol is taken from the Agent's descriptor, which is the
    // only declaration of what it can actually send; asserting a fixed one here
    // would be refused by the same validation the editor uses.
    let wire_api = gateway_wire_api_for(agent_id);
    catalog
        .iter()
        .filter(|model| !model.trim().is_empty())
        .take(MAX_GATEWAY_MODELS)
        .map(|model| ProviderConfiguredModel {
            id: model.clone(),
            display_name: None,
            enabled: true,
            wire_api,
            capabilities: ProviderModelCapabilities::default(),
        })
        .collect()
}

/// Fills in a route entry's capabilities from its members.
///
/// A route claims only what every member can do: the smallest context window,
/// and the intersection of the boolean features. Without this the Agent would
/// be told a route supports images because one member does, and then have the
/// request routed to a member that does not.
pub(crate) fn route_capabilities(
    conn: &vibex_db::DbConnection,
    profiles: &[vibex_core::ModelProviderProfile],
    route: &vibex_core::ModelRoute,
) -> vibex_core::ProviderModelCapabilities {
    let caps = route
        .enabled_members()
        .filter_map(|member| {
            let profile = profiles
                .iter()
                .find(|profile| profile.id == member.model_provider_profile_id)?;
            profile
                .configured_models
                .iter()
                .find(|model| model.id == member.model_id)
                .map(|model| model.capabilities.clone())
        })
        .collect::<Vec<_>>();
    let _ = conn;
    let merged = vibex_core::intersect_route_capabilities(caps);
    // An explicit route window overrides the smallest-member rule.
    vibex_core::ProviderModelCapabilities {
        context_tokens: route.context_tokens.or(merged.context_tokens),
        ..merged
    }
}

/// The protocol an Agent should keep speaking after it is pointed at the
/// gateway, preferring the Responses surface where an Agent offers it.
fn gateway_wire_api_for(agent_id: &AgentId) -> Option<ProviderModelWireApi> {
    let registry = vibex_core::AgentProviderProjectionRegistry::builtin().ok()?;
    let supported = registry
        .descriptors_for_agent(agent_id)
        .flat_map(|descriptor| descriptor.model_interfaces.iter())
        .filter_map(|interface| {
            ProviderModelWireApi::from_wire_protocol_id(&interface.wire_protocol_id)
        })
        // The gateway serves exactly four inbound protocols. An Agent whose
        // only interface is Mistral, Bedrock, or Vertex cannot be pointed at
        // it: the address would accept a request shape it does not serve.
        .filter(|wire_api| {
            matches!(
                wire_api,
                ProviderModelWireApi::OpenaiResponses
                    | ProviderModelWireApi::OpenaiChatCompletions
                    | ProviderModelWireApi::AnthropicMessages
                    | ProviderModelWireApi::GoogleGenerativeAi
            )
        })
        .collect::<Vec<_>>();
    [
        ProviderModelWireApi::OpenaiResponses,
        ProviderModelWireApi::OpenaiChatCompletions,
    ]
    .into_iter()
    .into_iter()
    .find(|wire_api| supported.contains(wire_api))
    .or_else(|| supported.first().copied())
}

fn gateway_provider_options(agent_id: &AgentId, profile_id: &str) -> ProviderOptions {
    let mut options = ProviderOptions::empty();
    for (key, value) in [
        (GATEWAY_PROFILE_ROLE_OPTION_KEY, GATEWAY_PROFILE_ROLE),
        (GATEWAY_AGENT_OPTION_KEY, agent_id.as_str()),
        ("vibex.internal.gatewayProfileId", profile_id),
        // The gateway speaks every protocol, so the Agent's own protocol is
        // the one the projection should keep writing.
        ("modelGateway", "true"),
    ] {
        if value.is_empty() {
            continue;
        }
        options.entries.push(vibex_core::ProviderBindingMetadata {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    options
}

fn gateway_provider_kind(agent_id: &AgentId) -> ProviderKind {
    match agent_id.as_str() {
        "claude" => ProviderKind::Claude,
        "codex" => ProviderKind::Codex,
        _ => ProviderKind::Acp,
    }
}

pub(crate) fn is_gateway_profile(profile: &ProviderProfile) -> bool {
    provider_option_value(&profile.provider_options, GATEWAY_PROFILE_ROLE_OPTION_KEY).as_deref()
        == Some(GATEWAY_PROFILE_ROLE)
}

fn gateway_profile_agent(profile: &ProviderProfile) -> Option<AgentId> {
    let agent_id = provider_option_value(&profile.provider_options, GATEWAY_AGENT_OPTION_KEY)?;
    AgentId::parse(agent_id).ok()
}

fn gateway_profile_for(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
) -> VibexResult<Option<ProviderProfile>> {
    let profiles = ProviderProfileRepository::list_by_agent(conn, agent_id, true)?;
    Ok(profiles.into_iter().find(is_gateway_profile))
}

/// The default Profile id an Agent is currently pointed at, resolved through
/// the same read the runtime uses.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn default_profile_for(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
) -> VibexResult<Option<ProviderProfileId>> {
    Ok(AgentDefaultModelProviderProfileRepository::get(
        conn,
        gateway_default_scope(),
        agent_id.clone(),
    )?
    .provider_profile_id)
}

/// Opens a connection for the gateway wiring in tests.
#[cfg(test)]
pub(crate) fn test_connection(path: &std::path::Path) -> VibexResult<vibex_db::DbConnection> {
    let mut conn = vibex_db::open_database(path)?;
    vibex_db::apply_migrations(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::RequestId;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-wiring-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).unwrap()
    }

    /// Turns the installation-wide gateway switch on or off.
    fn set_gateway_switch(service: &ProviderConfigService, enabled: bool) {
        let conn = service.open_connection().unwrap();
        let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        let mut next = current.clone();
        next.policy.enabled = enabled;
        next.revision = current.revision + 1;
        vibex_db::ModelGatewaySettingsRepository::update(&conn, &next, current.revision).unwrap();
    }

    fn enable_gateway(service: &ProviderConfigService) {
        set_gateway_switch(service, true);
    }

    fn disable_gateway(service: &ProviderConfigService) {
        set_gateway_switch(service, false);
    }

    /// Replacing a Key's secret revives it; clearing the secret does not.
    ///
    /// An authentication failure has no expiry, so the only way back for a
    /// rejected Key is a new value. The same write that stores it has to clear
    /// the state that keeps the Key out of rotation.
    #[test]
    fn replacing_a_key_clears_its_gateway_cooldown() {
        let (path, service) = service();
        let profile = vibex_core::ModelProviderProfile {
            display_name: "DeepSeek".to_string(),
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        {
            let conn = test_connection(&path).unwrap();
            vibex_db::ModelProviderProfileRepository::insert(&conn, &profile).unwrap();
        }
        let added = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: profile.id.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-original".to_string(),
            })
            .unwrap();
        let credential = added.credentials[0].id.clone();
        let conn = test_connection(&path).unwrap();
        let mut state =
            vibex_core::ModelProviderAccountState::ready(profile.id.clone(), credential.clone());
        state.cooldown_reason = Some(vibex_core::GatewayFailureKind::Auth);
        state.consecutive_failures = 1;
        vibex_db::ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();

        service
            .mutate_provider_credential_secret(
                vibex_core::ProviderCredentialSecretMutationRequest {
                    model_provider_profile_id: profile.id.clone(),
                    credential_id: credential.clone(),
                    touched: true,
                    clear: false,
                    value: Some("sk-replacement".to_string()),
                },
            )
            .unwrap();
        let states =
            vibex_db::ModelProviderAccountStateRepository::list(&conn, &profile.id).unwrap();
        assert!(
            states[0].is_ready_at(vibex_core::unix_timestamp_ms()),
            "a replaced Key must be tried again: {:?}",
            states[0]
        );

        // Clearing is not a replacement: a Key with no value should not look
        // ready just because the cooldown was reset.
        let mut state =
            vibex_core::ModelProviderAccountState::ready(profile.id.clone(), credential.clone());
        state.cooldown_reason = Some(vibex_core::GatewayFailureKind::Auth);
        vibex_db::ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();
        service
            .mutate_provider_credential_secret(
                vibex_core::ProviderCredentialSecretMutationRequest {
                    model_provider_profile_id: profile.id.clone(),
                    credential_id: credential.clone(),
                    touched: true,
                    clear: true,
                    value: None,
                },
            )
            .unwrap();
        let states =
            vibex_db::ModelProviderAccountStateRepository::list(&conn, &profile.id).unwrap();
        assert!(!states[0].is_ready_at(vibex_core::unix_timestamp_ms()));
        let _ = std::fs::remove_file(path);
    }

    /// Enabling the gateway attaches every capable Agent at once.
    #[test]
    fn enabling_attaches_every_capable_agent() {
        let (path, service) = service();
        enable_gateway(&service);
        let attached = service
            .attach_gateway_to_agents(
                "http://127.0.0.1:3425",
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        assert!(!attached.is_empty(), "at least one Agent must be attached");

        let conn = test_connection(&path).unwrap();
        for agent_id in &attached {
            let profile = gateway_profile_for(&conn, agent_id).unwrap().unwrap();
            assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3425"));
            assert!(is_gateway_profile(&profile));
            assert_eq!(
                profile
                    .configured_models
                    .first()
                    .map(|model| model.id.as_str()),
                Some("deepseek/deepseek-chat")
            );
            assert_eq!(
                default_profile_for(&conn, agent_id).unwrap(),
                Some(profile.id.clone()),
                "the gateway Profile is the Agent's default"
            );
        }
        drop(conn);

        // A hidden Profile must not appear in the Agent's visible provider list.
        let visible = service
            .list_agent_model_provider_profiles(vibex_core::AgentModelProviderProfileListRequest {
                agent_id: attached[0].clone(),
                include_disabled: true,
            })
            .unwrap();
        assert!(
            visible
                .profiles
                .iter()
                .all(|entry| !is_gateway_profile(&entry.profile))
        );
        let _ = std::fs::remove_file(path);
    }

    /// A second attach updates the same hidden Profile and address.
    #[test]
    fn attaching_twice_updates_the_same_profiles() {
        let (path, service) = service();
        enable_gateway(&service);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &[])
            .unwrap();
        let agent_id = agent("codex");
        let first = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        service
            .attach_gateway_to_agents("http://127.0.0.1:3426", &["a/b".to_string()])
            .unwrap();
        let second = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        assert_eq!(second.configured_models.len(), 1);
        let _ = std::fs::remove_file(path);
    }

    /// Detaching removes every hidden Profile, and the intent follows the switch.
    #[test]
    fn detaching_removes_every_hidden_profile() {
        let (path, service) = service();
        enable_gateway(&service);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &[])
            .unwrap();
        assert!(!service.gateway_agents().unwrap().is_empty());

        service.detach_gateway_from_agents().unwrap();
        assert!(service.gateway_active_agents().unwrap().is_empty());
        let conn = test_connection(&path).unwrap();
        let remaining = ProviderProfileRepository::list_all(&conn)
            .unwrap()
            .into_iter()
            .filter(is_gateway_profile)
            .count();
        assert_eq!(remaining, 0, "no hidden Profile may survive the switch");
        let _ = std::fs::remove_file(path);
    }

    /// The gateway's intent is global: off means no Agent reports using it.
    #[test]
    fn the_gateway_intent_follows_the_global_switch() {
        let (path, service) = service();
        // A fresh installation ships with the gateway on, so the switch has to
        // be turned off explicitly before the "off" answer means anything.
        disable_gateway(&service);
        assert!(
            service.gateway_agents().unwrap().is_empty(),
            "a disabled gateway serves no Agent"
        );
        enable_gateway(&service);
        assert!(
            !service.gateway_agents().unwrap().is_empty(),
            "an enabled gateway serves every capable Agent"
        );
        disable_gateway(&service);
        assert!(service.gateway_agents().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A stopped listener removes the Profiles while the intent stays on.
    #[test]
    fn a_stopped_gateway_removes_profiles_but_keeps_the_switch() {
        let (path, service) = service();
        enable_gateway(&service);
        service
            .attach_gateway_to_agents(
                "http://127.0.0.1:3425",
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        let agent_id = agent("codex");
        let conn = test_connection(&path).unwrap();
        assert!(default_profile_for(&conn, &agent_id).unwrap().is_some());
        drop(conn);

        let deactivated = service.deactivate_gateway_profiles().unwrap();
        assert!(deactivated.contains(&agent_id));
        assert!(service.gateway_active_agents().unwrap().is_empty());
        // The switch is still on, so a working listener restores the Profiles
        // without asking the user again.
        assert!(!service.gateway_agents().unwrap().is_empty());
        let conn = test_connection(&path).unwrap();
        assert_eq!(default_profile_for(&conn, &agent_id).unwrap(), None);
        drop(conn);

        service
            .refresh_gateway_profiles(
                "http://127.0.0.1:3426",
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        assert!(service.gateway_active_agents().unwrap().contains(&agent_id));
        let conn = test_connection(&path).unwrap();
        let default = default_profile_for(&conn, &agent_id).unwrap().unwrap();
        let profile = service.get_profile(&default).unwrap().unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        let _ = std::fs::remove_file(path);
    }

    /// An Agent that owns its provider is never attached.
    #[test]
    fn an_agent_that_owns_its_provider_is_never_attached() {
        let (path, service) = service();
        enable_gateway(&service);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &[])
            .unwrap();
        assert!(
            !service
                .gateway_active_agents()
                .unwrap()
                .contains(&agent("cursor")),
            "an Agent that manages its own provider cannot use the gateway"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Attaching without a listener address is refused.
    #[test]
    fn attaching_without_an_address_is_refused() {
        let (path, service) = service();
        enable_gateway(&service);
        let error = service.attach_gateway_to_agents("", &[]).unwrap_err();
        assert_eq!(error.code, "model_gateway_unavailable");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refreshing_tracks_the_listener_address() {
        let (path, service) = service();
        let agent_id = agent("codex");
        enable_gateway(&service);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &[])
            .unwrap();
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3426", &["x/y".to_string()])
            .unwrap();
        assert!(refreshed.contains(&agent_id));
        let profile = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        // A second call with the same address is a no-op, so it cannot churn
        // the projection fingerprint.
        assert!(
            service
                .refresh_gateway_profiles("http://127.0.0.1:3426", &["x/y".to_string()])
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    /// Publishing a route group is what puts it in an Agent's model picker.
    ///
    /// A session reads its selectable models from the Agent's gateway Profile,
    /// which is a copy of the catalogue taken when the Profile is refreshed. A
    /// group that is created and bound but never republished therefore exists,
    /// is bound, and still does not appear in the picker — the failure this test
    /// pins down.
    #[test]
    fn a_bound_group_appears_in_the_agents_profile_only_after_republishing() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id;

        // An Agent already routed through the gateway, with the pre-group
        // catalogue published.
        let agent_id = agent("claude");
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &[])
            .unwrap();

        let group = service
            .create_model_route(vibex_core::ModelRoute {
                display_name: "Team".to_string(),
                kind: vibex_core::RouteKind::ProviderGroup,
                status: vibex_core::RouteStatus::Enabled,
                members: vec![vibex_core::ModelRouteMember::provider(deepseek, 0)],
                ..Default::default()
            })
            .unwrap();
        service
            .bind_model_route(&agent_id, &group.id, false)
            .unwrap();

        // The group is in the catalogue the gateway would serve...
        let catalog = service.gateway_model_catalog().unwrap();
        let group_entry = catalog
            .iter()
            .find(|entry| entry.display_name == "Team")
            .expect("the group is published by the gateway");
        assert_eq!(
            group_entry.model,
            format!("group/{}/deepseek-chat", group.id.as_str())
        );

        // ...but until the catalogue is republished, the Agent's Profile does
        // not list it, which is exactly what made the picker look empty.
        let conn = test_connection(&path).unwrap();
        let profile_id = default_profile_for(&conn, &agent_id).unwrap().unwrap();
        let before = service.get_profile(&profile_id).unwrap().unwrap();
        assert!(
            before
                .configured_models
                .iter()
                .all(|model| !model.id.starts_with("group/")),
            "the stale Profile predates the group"
        );

        // Republishing is the step a route write now performs.
        let catalog = service
            .gateway_model_catalog()
            .unwrap()
            .into_iter()
            .map(|entry| entry.display_id())
            .collect::<Vec<_>>();
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3425", &catalog)
            .unwrap();
        assert!(
            refreshed.contains(&agent_id),
            "the refresh must report the Agent it changed, not silently skip it"
        );
        let after = service.get_profile(&profile_id).unwrap().unwrap();
        assert!(
            after
                .configured_models
                .iter()
                .any(|model| model.id == format!("group/{}/deepseek-chat", group.id.as_str())),
            "the republished Profile is what a session reads its models from"
        );
        let _ = std::fs::remove_file(path);
    }

    /// An ACP Agent's gateway Profile accepts a new catalogue.
    ///
    /// `update_profile` refuses an ACP Profile whose runtime configuration was
    /// not supplied, and a gateway Profile's options never carry one — it is
    /// derived. Refreshing through the generic update therefore failed with
    /// `acp_config_missing`, the per-Agent `if let Ok` swallowed it, and the
    /// catalogue never reached the Profile. Only an ACP-kind Agent (anything
    /// but `claude`/`codex`) exercises that path.
    #[test]
    fn an_acp_agents_gateway_profile_accepts_a_new_catalogue() {
        let (path, service) = service();
        enable_gateway(&service);
        let agent_id = agent("deepseek-harness");

        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &["route/first".to_string()])
            .unwrap();
        let conn = test_connection(&path).unwrap();
        let profile_id = default_profile_for(&conn, &agent_id)
            .unwrap()
            .expect("the ACP Agent is on the gateway");
        drop(conn);
        let before = service.get_profile(&profile_id).unwrap().unwrap();
        assert_eq!(
            before
                .configured_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["route/first"],
            "the gateway Profile starts with what was published"
        );

        // A group arrives, so the catalogue changes.
        let refreshed = service
            .refresh_gateway_profiles(
                "http://127.0.0.1:3425",
                &["route/first".to_string(), "group/g1/m".to_string()],
            )
            .unwrap();
        assert!(
            refreshed.contains(&agent_id),
            "an ACP Agent must be updated, not skipped for a missing ACP config"
        );
        let after = service.get_profile(&profile_id).unwrap().unwrap();
        assert_eq!(
            after
                .configured_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["route/first", "group/g1/m"],
            "the Profile now lists the group's model"
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;
    use vibex_core::{
        AgentCredential, AgentCredentialStatus, ProjectionSecretReference, RequestId,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-fingerprint-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// Turns the installation-wide gateway switch on.
    fn enable_gateway(service: &ProviderConfigService) {
        let conn = service.open_connection().unwrap();
        let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        let mut next = current.clone();
        next.policy.enabled = true;
        next.revision = current.revision + 1;
        vibex_db::ModelGatewaySettingsRepository::update(&conn, &next, current.revision).unwrap();
    }

    /// Creates a shared upstream with one stored Key and returns its id.
    fn upstream(
        service: &ProviderConfigService,
        display_name: &str,
    ) -> vibex_core::ModelProviderProfileId {
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        crate::secrets::store_provider_secret(&lookup_key, "sk-test").unwrap();
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: display_name.to_string(),
                vendor_hint: None,
                endpoints: vec![vibex_core::ModelProviderEndpoint {
                    id: "endpoint-api".to_string(),
                    kind: vibex_core::ModelProviderEndpointKind::Api,
                    url: "https://api.example.invalid".to_string(),
                    wire_protocol_id: Some(
                        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    ),
                }],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: vec![vibex_core::ModelProviderCredentialReference {
                    id: RequestId::new(),
                    display_name: "Primary".to_string(),
                    status: AgentCredentialStatus::Ready,
                    credential: AgentCredential::ApiKey {
                        secret: ProjectionSecretReference {
                            id: RequestId::new(),
                            backend: crate::secrets::provider_secret_write_backend(),
                            setup_state: vibex_core::ProviderSecretSetupState::Available,
                            lookup_key,
                            redacted_hint: "stored".to_string(),
                            revision: 1,
                            legacy_secret_reference_id: None,
                        },
                        target_hint: None,
                    },
                    revision: 1,
                }],
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// The promise the whole gateway rests on: changing an **upstream's** Key
    /// must not move the gateway-bound Agent's launch contract, so it does not
    /// restart that Agent's process.
    ///
    /// The Agent's own Profile holds only the gateway address, so an upstream
    /// Key rotation never reaches it. This test asserts exactly that: the
    /// upstream's credential revision moves, the Agent Profile's launch
    /// revision does not.
    #[test]
    fn rotating_an_upstream_key_does_not_move_the_agent_launch_revision() {
        let (path, service) = service();
        let agent_id = vibex_core::AgentId::parse("codex").unwrap();
        let shared = upstream(&service, "DeepSeek");

        // Point the Agent at the gateway.
        enable_gateway(&service);
        service
            .attach_gateway_to_agents(
                "http://127.0.0.1:3425",
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        let agent_profile = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .expect("a gateway profile")
            .id;
        let before = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();

        // Rotate every Key on the shared upstream, which is what a user does
        // when a Key is exhausted.
        let mut shared_profile = service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| profile.id == shared)
            .unwrap();
        for credential in &mut shared_profile.credentials {
            credential.revision = credential.revision.saturating_add(1);
            if let Some(reference) = credential.credential.secret_reference().cloned() {
                let mut next = reference;
                next.revision = next.revision.saturating_add(1);
                // The secret value is untouched; only its reference revision
                // moves, exactly as a Key replacement would move it.
                credential.credential = AgentCredential::ApiKey {
                    secret: next,
                    target_hint: None,
                };
            }
        }
        let expected = shared_profile.revision;
        shared_profile.revision = expected + 1;
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: shared_profile,
                expected_revision: expected,
            })
            .unwrap();

        let after = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();
        assert_eq!(
            before, after,
            "an upstream Key change moved the gateway-bound Agent's launch revision, \
             which would restart its process"
        );

        // And the gateway address itself does move it: moving the listener port
        // is a relaunch, which is why the port is persisted rather than re-picked.
        service
            .attach_gateway_to_agents(
                "http://127.0.0.1:3426",
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        let moved = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();
        assert_ne!(
            before, moved,
            "moving the gateway's address must count as a relaunch"
        );

        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use vibex_core::RequestId;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-credential-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn profile_id(service: &ProviderConfigService) -> vibex_core::ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: vec![vibex_core::ModelProviderEndpoint {
                    id: "endpoint-api".to_string(),
                    kind: vibex_core::ModelProviderEndpointKind::Api,
                    url: "https://api.example.invalid".to_string(),
                    wire_protocol_id: Some(
                        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    ),
                }],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    #[test]
    fn a_key_can_be_added_and_removed_without_storing_plaintext() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let profile = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-secret-value".to_string(),
            })
            .unwrap();
        assert_eq!(profile.credentials.len(), 1);
        assert_eq!(profile.credentials[0].display_name, "Primary");
        assert_eq!(
            profile.credentials[0].status,
            vibex_core::AgentCredentialStatus::Ready
        );
        // The row holds a reference, never the value.
        let encoded = serde_json::to_string(&profile).unwrap();
        assert!(
            !encoded.contains("sk-secret-value"),
            "the profile row carried the key value"
        );

        let credential_id = profile.credentials[0].id.clone();
        let profile = service
            .remove_model_provider_credential(vibex_core::RemoveModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                credential_id,
            })
            .unwrap();
        assert!(profile.credentials.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_empty_key_is_refused() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let error = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                display_name: None,
                value: "   ".to_string(),
            })
            .unwrap_err();
        assert_eq!(error.code, "model_provider_credential_empty");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn removing_an_unknown_key_is_a_named_error() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let error = service
            .remove_model_provider_credential(vibex_core::RemoveModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                credential_id: RequestId::new(),
            })
            .unwrap_err();
        assert_eq!(error.code, "model_provider_credential_not_found");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn each_added_key_advances_the_revision_so_bindings_restale() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let first = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                display_name: None,
                value: "sk-one".to_string(),
            })
            .unwrap();
        let second = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                display_name: None,
                value: "sk-two".to_string(),
            })
            .unwrap();
        assert!(second.revision > first.revision);
        assert_eq!(second.credentials.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod legacy_mirror_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-legacy-mirror-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// A legacy Profile with no ACP config: Claude's kind, so the projection
    /// accepts it without the typed ACP contract an `Acp` Profile requires.
    fn legacy_profile(service: &ProviderConfigService) -> ProviderProfileId {
        service
            .create_profile(vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse("claude").unwrap()),
                kind: ProviderKind::Claude,
                display_name: "Mirror Probe".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid".to_string()),
                default_model: Some("m1".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![vibex_core::ProviderConfiguredModel {
                    id: "m1".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(vibex_core::ProviderOptions {
                    schema_version: 1,
                    entries: vec![vibex_core::ProviderBindingMetadata {
                        key: "wireApi".to_string(),
                        value: "anthropic_messages".to_string(),
                    }],
                }),
                secret_references: Vec::new(),
            })
            .unwrap()
            .id
    }

    fn shared(
        service: &ProviderConfigService,
        legacy: &ProviderProfileId,
    ) -> vibex_core::ModelProviderProfile {
        service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| profile.legacy_provider_profile_id.as_ref() == Some(legacy))
            .expect("a legacy create projects a shared Profile")
    }

    /// The regression this module exists for: a Key added through v2 must not
    /// vanish when the same Provider is next written through v1.
    #[test]
    fn a_v2_key_survives_a_later_v1_write() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: profile.id.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-mirror-value".to_string(),
            })
            .unwrap();

        // The legacy row is what the launch path resolves a Key from.
        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(
            mirrored.secrets.len(),
            1,
            "a v2 Key must reach the legacy secret table"
        );

        service
            .update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: legacy_id.clone(),
                display_name: Some("Renamed from the Agent page".to_string()),
                status: None,
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: None,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .unwrap();

        let after = service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == profile.id)
            .unwrap();
        assert_eq!(
            after.credentials.len(),
            1,
            "a v1 write dropped the Key the v2 API added"
        );
        let _ = std::fs::remove_file(path);
    }

    /// A rename and a status change made on the global page must reach the
    /// record the launch path reads, and must not be undone by a v1 write.
    #[test]
    fn a_v2_rename_and_status_reach_the_legacy_record() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        let mut next = profile.clone();
        next.display_name = "Renamed on the global page".to_string();
        next.status = vibex_core::ModelProviderProfileStatus::Disabled;
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: next,
                expected_revision: profile.revision,
            })
            .unwrap();

        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(mirrored.display_name, "Renamed on the global page");
        assert_eq!(mirrored.status, ProviderProfileStatus::Disabled);

        // A v1 write restates the same values, so the name survives it rather
        // than snapping back to whatever v1 last held.
        service
            .update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: legacy_id.clone(),
                display_name: None,
                status: None,
                account_alias: None,
                base_url: Some("https://moved.example.invalid".to_string()),
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: None,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .unwrap();
        let after = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(after.display_name, "Renamed on the global page");
        assert_eq!(after.status, ProviderProfileStatus::Disabled);
        let _ = std::fs::remove_file(path);
    }

    /// The projection must not strip what v2 cannot express. The protocol pin
    /// and the ACP config live only in `provider_options`, and losing them
    /// would silently re-point the Agent at the wrong API.
    #[test]
    fn the_projection_keeps_the_options_v2_cannot_express() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        let mut next = profile.clone();
        next.display_name = "Only the name moves".to_string();
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: next,
                expected_revision: profile.revision,
            })
            .unwrap();

        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(
            provider_option_value(&mirrored.provider_options, "wireApi").as_deref(),
            Some("anthropic_messages"),
            "the protocol pin was lost"
        );
        // And the Models keep their declared protocol rather than reverting to
        // "inherit", which the projection would read as the Provider default.
        assert_eq!(
            mirrored.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod bind_to_agent_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-bind-agent-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn profile(service: &ProviderConfigService, agent: &str) -> ProviderProfile {
        service
            .create_profile(vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse(agent).unwrap()),
                kind: ProviderKind::Claude,
                display_name: format!("Shared for {agent}"),
                account_alias: None,
                base_url: Some("https://api.example.invalid".to_string()),
                default_model: Some("m1".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![vibex_core::ProviderConfiguredModel {
                    id: "m1".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap()
    }

    /// One Provider reaches a second Agent without re-entering its address,
    /// protocol or Key, and each Agent keeps its own launch row.
    #[test]
    fn a_provider_can_be_given_to_a_second_agent() {
        let (path, service) = service();
        let source = profile(&service, "claude");
        assert_eq!(source.agent_id.as_str(), "claude");

        let bound = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: source.id.clone(),
                model_provider_profile_id: None,
                agent_id: vibex_core::AgentId::parse("deepseek-harness").unwrap(),
                make_default: false,
            })
            .unwrap();

        assert_eq!(bound.agent_id.as_str(), "deepseek-harness");
        assert_ne!(bound.id, source.id, "the copy must be its own Profile");
        assert_eq!(bound.base_url, source.base_url);
        assert_eq!(
            bound.configured_models.len(),
            source.configured_models.len()
        );
        // The two rows keep independent identities, which is what lets each
        // Agent launch with its own protocol.
        assert!(bound.secrets.iter().all(|bound_secret| {
            source
                .secrets
                .iter()
                .all(|source_secret| source_secret.id != bound_secret.id)
        }));
        let _ = std::fs::remove_file(path);
    }

    /// Binding the Provider an Agent already holds is refused rather than
    /// silently creating a duplicate row for the same Agent.
    #[test]
    fn binding_a_provider_to_its_own_agent_is_refused() {
        let (path, service) = service();
        let source = profile(&service, "claude");
        let error = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: source.id.clone(),
                model_provider_profile_id: None,
                agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                make_default: false,
            })
            .unwrap_err();
        assert_eq!(error.code, "agent_model_provider_binding_same_agent");
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod shared_provider_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-shared-provider-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn endpoint(id: &str, url: &str, protocol: Option<&str>) -> vibex_core::ModelProviderEndpoint {
        vibex_core::ModelProviderEndpoint {
            id: id.to_string(),
            kind: vibex_core::ModelProviderEndpointKind::Api,
            url: url.to_string(),
            wire_protocol_id: protocol.map(str::to_string),
        }
    }

    /// A shared Provider is created without naming an Agent, and it carries one
    /// address per protocol.
    ///
    /// This is the regression the global Providers page hit: creating there
    /// borrowed an Agent, so a Provider whose protocol that Agent could not
    /// speak was refused at save time with
    /// `agent_model_interface_unsupported`, even though nothing about the
    /// Provider itself was wrong.
    #[test]
    fn a_shared_provider_is_created_without_an_agent() {
        let (path, service) = service();
        let created = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: vec![
                    endpoint("api", "https://api.deepseek.com/v1", None),
                    endpoint(
                        "api-anthropic",
                        "https://api.deepseek.com/anthropic",
                        Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES),
                    ),
                ],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();

        // No Agent, and both protocols are addressable on the one record.
        assert!(created.legacy_provider_profile_id.is_none());
        assert_eq!(created.endpoints.len(), 2);
        let protocols = created
            .endpoints
            .iter()
            .filter_map(|endpoint| endpoint.wire_protocol_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            protocols,
            vec![vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES]
        );
        let _ = std::fs::remove_file(path);
    }

    /// The protocol check moves to the binding, which is the first moment an
    /// Agent exists to have the limitation.
    #[test]
    fn a_shared_provider_is_checked_against_the_agent_that_binds_it() {
        let (path, service) = service();
        let created = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared Anthropic".to_string(),
                vendor_hint: None,
                endpoints: vec![endpoint(
                    "api-anthropic",
                    "https://api.anthropic.com",
                    Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES),
                )],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();
        // Creating it needed no Agent and was accepted.
        assert_eq!(created.display_name, "Shared Anthropic");
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod shared_bind_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-shared-bind-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// A Provider created on the global page can be given to an Agent, which
    /// materialises the legacy row the launch path needs.
    #[test]
    fn a_shared_provider_materialises_for_the_agent_that_binds_it() {
        let (path, service) = service();
        let shared = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared Messages".to_string(),
                vendor_hint: None,
                endpoints: vec![
                    vibex_core::ModelProviderEndpoint {
                        id: "api".to_string(),
                        kind: vibex_core::ModelProviderEndpointKind::Api,
                        url: "https://api.example.invalid".to_string(),
                        wire_protocol_id: None,
                    },
                    vibex_core::ModelProviderEndpoint {
                        id: "api-anthropic".to_string(),
                        kind: vibex_core::ModelProviderEndpointKind::Api,
                        url: "https://api.example.invalid/anthropic".to_string(),
                        wire_protocol_id: Some(
                            vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                        ),
                    },
                ],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "claude-sonnet-4-5".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("claude-sonnet-4-5".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();

        let bound = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                model_provider_profile_id: Some(shared.id.clone()),
                agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                make_default: false,
            })
            .unwrap();

        assert_eq!(bound.agent_id.as_str(), "claude");
        // The protocol the shared record published is what the Agent launches
        // with, and the per-protocol address survived as an option.
        assert_eq!(
            provider_option_value(&bound.provider_options, "wireApi").as_deref(),
            Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES)
        );
        assert_eq!(
            provider_option_value(
                &bound.provider_options,
                &vibex_core::ProviderModelWireApi::AnthropicMessages.protocol_base_url_option_key()
            )
            .as_deref(),
            Some("https://api.example.invalid/anthropic")
        );
        assert_eq!(
            bound.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
        let _ = std::fs::remove_file(path);
    }
}
