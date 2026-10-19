//! Per-Agent wiring for the local model gateway.
//!
//! The gateway is a single shared endpoint, but an Agent is pointed at an
//! address through its own Provider Profile. This module bridges the two: for
//! each Agent the user switched to the gateway it maintains one hidden legacy
//! Profile whose base URL is the gateway, keeps its model list in step with the
//! aggregated catalogue, and makes it the Agent's default.
//!
//! The Profiles are marked with the internal role so they never appear in an
//! Agent's provider list: the gateway is configured in one place and consumed
//! here, rather than being duplicated into every Agent's editor.

use std::collections::BTreeSet;

use vibex_core::{
    AgentId, GatewayRoutingStrategy, ModelProviderProfileId, ModelProviderProfileStatus,
    ProviderConfiguredModel, ProviderKind, ProviderModelCapabilities, ProviderModelWireApi,
    ProviderOptions, ProviderProfile, ProviderProfileCreateRequest, ProviderProfileId,
    ProviderProfileStatus, ProviderSecretBackend, ProviderSecretKind,
    ProviderSecretReferenceCreateRequest, ProviderSecretSetupState, VibexError, VibexResult,
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
    /// The Agents currently configured to walk through the gateway.
    pub fn gateway_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
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

    /// Every gateway-capable Agent and whether it currently uses the gateway.
    ///
    /// An Agent that manages its own provider cannot be routed through the
    /// gateway at all, so it is reported as unsupported rather than silently
    /// omitted.
    pub fn gateway_agent_options(
        &self,
    ) -> VibexResult<Vec<(AgentId, GatewayRoutingStrategy, bool)>> {
        let configurable = vibex_core::model_provider_configurable_agent_ids()?;
        let enabled = self.gateway_agents()?;
        let conn = self.open_connection()?;
        let mut options = Vec::new();
        for agent_id in configurable {
            // The gateway always speaks whichever protocol the Agent does, so
            // the routing strategy is the only per-Agent choice it carries.
            let strategy = gateway_profile_for(&conn, &agent_id)?
                .map(|profile| gateway_routing_of(&profile))
                .unwrap_or_default();
            options.push((agent_id.clone(), strategy, enabled.contains(&agent_id)));
        }
        options.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        Ok(options)
    }

    /// Points one Agent at the gateway, or takes it back to a direct Provider.
    ///
    /// Enabling creates or refreshes the hidden Profile and makes it the
    /// Agent's default; disabling deletes it, which lets the Agent fall back to
    /// whatever direct Profile it had. `base_url` is the address the gateway is
    /// currently listening on.
    pub fn set_gateway_agent(
        &self,
        agent_id: &AgentId,
        enabled: bool,
        base_url: Option<&str>,
        model_catalog: &[String],
    ) -> VibexResult<Option<ProviderProfileId>> {
        if !vibex_core::model_provider_configurable_agent_ids()?.contains(agent_id) {
            return Err(VibexError::validation(
                "model_gateway_agent_unsupported",
                "this Agent manages its own provider and cannot use the gateway",
            )
            .with_diagnostic("agentId", agent_id.as_str()));
        }
        let conn = self.open_connection()?;
        let existing = gateway_profile_for(&conn, agent_id)?;
        if !enabled {
            if let Some(profile) = existing {
                drop(conn);
                self.delete_profile(vibex_core::ProviderProfileDeleteRequest {
                    provider_profile_id: profile.id,
                })?;
            }
            return Ok(None);
        }
        let base_url = base_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                VibexError::validation(
                    "model_gateway_unavailable",
                    "the gateway is not running, so no Agent can be pointed at it",
                )
            })?;
        let models = gateway_models(agent_id, model_catalog);
        match existing {
            Some(profile) => {
                let profile_id = profile.id.clone();
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
                    provider_options: Some(gateway_provider_options(agent_id, profile_id.as_str())),
                })?;
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok(Some(profile_id))
            }
            None => {
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
                    provider_options: Some(gateway_provider_options(agent_id, "")),
                    secret_references: vec![ProviderSecretReferenceCreateRequest {
                        secret_kind: ProviderSecretKind::ApiKey,
                        backend: ProviderSecretBackend::Placeholder,
                        setup_state: ProviderSecretSetupState::Referenced,
                        lookup_key: format!("vibex-model-gateway-{}", agent_id.as_str()),
                        display_label: "Vibex Gateway".to_string(),
                        redacted_hint: GATEWAY_LOCAL_KEY_HINT.to_string(),
                    }],
                })?;
                let options = gateway_provider_options(agent_id, created.id.as_str());
                let profile_id = created.id.clone();
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
                    provider_options: Some(options),
                })?;
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok(Some(profile_id))
            }
        }
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
        let agents = self.gateway_agents()?;
        let mut refreshed = Vec::new();
        for agent_id in agents {
            let conn = self.open_connection()?;
            let Some(profile) = gateway_profile_for(&conn, &agent_id)? else {
                continue;
            };
            let models = gateway_models(&agent_id, model_catalog);
            let unchanged = profile.base_url.as_deref() == Some(base_url)
                && profile.configured_models == models;
            if unchanged {
                continue;
            }
            let profile_id = profile.id.clone();
            drop(conn);
            self.update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: profile_id,
                display_name: Some(gateway_display_name()),
                status: None,
                account_alias: None,
                base_url: Some(base_url.to_string()),
                default_model: models.first().map(|model| model.id.clone()),
                small_model: None,
                large_model: None,
                configured_models: Some(models.clone()),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })?;
            refreshed.push(agent_id);
        }
        Ok(refreshed)
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
    pub fn gateway_model_catalog(&self) -> VibexResult<Vec<GatewayCatalogModel>> {
        let conn = self.open_connection()?;
        let settings = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?;
        let profiles = ModelProviderProfileRepository::list(&conn)?;
        let mut catalog = Vec::new();
        for upstream_id in &settings.upstreams {
            let Some(profile) = profiles.iter().find(|profile| &profile.id == upstream_id) else {
                continue;
            };
            if profile.status != ModelProviderProfileStatus::Enabled {
                continue;
            }
            for model in profile
                .configured_models
                .iter()
                .filter(|model| model.enabled)
            {
                catalog.push(GatewayCatalogModel {
                    provider: upstream_id.clone(),
                    provider_name: profile.display_name.clone(),
                    model: model.id.clone(),
                });
            }
        }
        Ok(catalog)
    }
}

/// One model the gateway serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCatalogModel {
    pub provider: ModelProviderProfileId,
    pub provider_name: String,
    pub model: String,
}

impl GatewayCatalogModel {
    /// The id an Agent's model list shows: the upstream's short name and the
    /// model id.
    pub fn display_id(&self) -> String {
        format!("{}/{}", self.provider_name, self.model)
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

fn is_gateway_profile(profile: &ProviderProfile) -> bool {
    provider_option_value(&profile.provider_options, GATEWAY_PROFILE_ROLE_OPTION_KEY).as_deref()
        == Some(GATEWAY_PROFILE_ROLE)
}

fn gateway_profile_agent(profile: &ProviderProfile) -> Option<AgentId> {
    let agent_id = provider_option_value(&profile.provider_options, GATEWAY_AGENT_OPTION_KEY)?;
    AgentId::parse(agent_id).ok()
}

fn gateway_routing_of(_profile: &ProviderProfile) -> GatewayRoutingStrategy {
    // The routing strategy lives in the gateway settings, not on each Profile;
    // the per-Agent row reports the gateway's own setting through the service.
    GatewayRoutingStrategy::Ordered
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

    #[test]
    fn enabling_creates_one_hidden_profile_and_makes_it_the_default() {
        let (path, service) = service();
        let agent_id = agent("codex");
        let profile_id = service
            .set_gateway_agent(
                &agent_id,
                true,
                Some("http://127.0.0.1:3425"),
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap()
            .expect("a profile id");
        assert!(service.gateway_agents().unwrap().contains(&agent_id));

        let profile = service.get_profile(&profile_id).unwrap().unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3425"));
        assert!(is_gateway_profile(&profile));
        assert_eq!(
            profile
                .configured_models
                .first()
                .map(|model| model.id.as_str()),
            Some("deepseek/deepseek-chat")
        );

        let conn = test_connection(&path).unwrap();
        assert_eq!(
            default_profile_for(&conn, &agent_id).unwrap(),
            Some(profile_id.clone())
        );
        // A hidden Profile must not appear in the Agent's visible provider list.
        let visible = service
            .list_agent_model_provider_profiles(vibex_core::AgentModelProviderProfileListRequest {
                agent_id: agent_id.clone(),
                include_disabled: true,
            })
            .unwrap();
        assert!(
            visible
                .profiles
                .iter()
                .all(|entry| entry.profile.id != profile_id)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn enabling_twice_updates_the_same_profile_and_address() {
        let (path, service) = service();
        let agent_id = agent("codex");
        let first = service
            .set_gateway_agent(&agent_id, true, Some("http://127.0.0.1:3425"), &[])
            .unwrap()
            .unwrap();
        let second = service
            .set_gateway_agent(
                &agent_id,
                true,
                Some("http://127.0.0.1:3426"),
                &["a/b".to_string()],
            )
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        let profile = service.get_profile(&second).unwrap().unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        assert_eq!(profile.configured_models.len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn disabling_removes_the_profile_and_the_agent_choice() {
        let (path, service) = service();
        let agent_id = agent("codex");
        service
            .set_gateway_agent(&agent_id, true, Some("http://127.0.0.1:3425"), &[])
            .unwrap();
        assert!(service.gateway_agents().unwrap().contains(&agent_id));
        service
            .set_gateway_agent(&agent_id, false, None, &[])
            .unwrap();
        assert!(!service.gateway_agents().unwrap().contains(&agent_id));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_agent_that_owns_its_provider_is_refused() {
        let (path, service) = service();
        let error = service
            .set_gateway_agent(&agent("cursor"), true, Some("http://127.0.0.1:3425"), &[])
            .unwrap_err();
        assert_eq!(error.code, "model_gateway_agent_unsupported");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn enabling_without_a_running_gateway_is_refused() {
        let (path, service) = service();
        let error = service
            .set_gateway_agent(&agent("codex"), true, None, &[])
            .unwrap_err();
        assert_eq!(error.code, "model_gateway_unavailable");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refreshing_tracks_the_listener_address() {
        let (path, service) = service();
        let agent_id = agent("codex");
        service
            .set_gateway_agent(&agent_id, true, Some("http://127.0.0.1:3425"), &[])
            .unwrap();
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3426", &["x/y".to_string()])
            .unwrap();
        assert_eq!(refreshed, vec![agent_id.clone()]);
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
}
