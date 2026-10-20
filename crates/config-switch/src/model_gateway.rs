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
    AgentDefaultModelProviderProfileRepository, ModelGatewayAgentRepository,
    ModelProviderProfileRepository, ProviderProfileRepository,
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
    /// The Agents the user has switched to the gateway.
    ///
    /// The answer is the durable choice, not the current projection: while the
    /// listener is down the hidden Profiles are removed so the Agents go back
    /// to a direct Provider, but these rows stay so a working listener restores
    /// them without asking again.
    pub fn gateway_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        let conn = self.open_connection()?;
        Ok(ModelGatewayAgentRepository::list(&conn)?
            .into_iter()
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
        if !enabled {
            let conn = self.open_connection()?;
            let existing = gateway_profile_for(&conn, agent_id)?;
            drop(conn);
            if let Some(profile) = existing {
                self.delete_profile(vibex_core::ProviderProfileDeleteRequest {
                    provider_profile_id: profile.id,
                })?;
            }
            let conn = self.open_connection()?;
            ModelGatewayAgentRepository::remove(&conn, agent_id)?;
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
        if gateway_wire_api_for(agent_id).is_none() {
            return Err(VibexError::validation(
                "model_gateway_agent_unsupported",
                "this Agent speaks a wire protocol the gateway does not serve",
            )
            .with_diagnostic("agentId", agent_id.as_str()));
        }
        let (profile_id, _) = self.ensure_gateway_profile(agent_id, base_url, model_catalog)?;
        let conn = self.open_connection()?;
        ModelGatewayAgentRepository::set(&conn, agent_id, true)?;
        Ok(Some(profile_id))
    }

    /// Removes every hidden gateway Profile while keeping the durable choice.
    ///
    /// This is what a stopped listener has to do: an Agent whose Profile still
    /// points at a dead loopback port cannot fall back, so the Profile goes
    /// away and the Agent returns to its direct default. `gateway_agents()`
    /// still remembers the Agent, so a later start re-creates the Profile.
    pub fn deactivate_gateway_profiles(&self) -> VibexResult<Vec<AgentId>> {
        let agents = self.gateway_agents()?;
        let mut deactivated = Vec::new();
        for agent_id in agents {
            let conn = self.open_connection()?;
            let Some(profile) = gateway_profile_for(&conn, &agent_id)? else {
                continue;
            };
            drop(conn);
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
        let existing = gateway_profile_for(&conn, agent_id)?;
        match existing {
            Some(profile) => {
                let unchanged = profile.base_url.as_deref() == Some(base_url)
                    && profile.configured_models == models
                    && profile.status == ProviderProfileStatus::Enabled;
                let profile_id = profile.id.clone();
                drop(conn);
                if !unchanged {
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
                        provider_options: Some(gateway_provider_options(
                            agent_id,
                            profile_id.as_str(),
                        )),
                    })?;
                }
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, !unchanged))
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
                    provider_options: Some(gateway_provider_options(agent_id, profile_id.as_str())),
                })?;
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, true))
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
            let (_, changed) = self.ensure_gateway_profile(&agent_id, base_url, model_catalog)?;
            if changed {
                refreshed.push(agent_id);
            }
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
    fn a_stopped_gateway_removes_profiles_but_remembers_the_agents() {
        let (path, service) = service();
        let agent_id = agent("codex");
        service
            .set_gateway_agent(
                &agent_id,
                true,
                Some("http://127.0.0.1:3425"),
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap();
        let conn = test_connection(&path).unwrap();
        assert!(default_profile_for(&conn, &agent_id).unwrap().is_some());
        drop(conn);

        let deactivated = service.deactivate_gateway_profiles().unwrap();
        assert_eq!(deactivated, vec![agent_id.clone()]);
        // The durable choice survives, so bringing the listener back needs no
        // second decision from the user.
        assert!(service.gateway_agents().unwrap().contains(&agent_id));
        assert!(service.gateway_active_agents().unwrap().is_empty());
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

    /// Creates a shared upstream with one stored Key and returns its id.
    fn upstream(service: &ProviderConfigService, display_name: &str) -> ModelProviderProfileId {
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
        let agent_profile = service
            .set_gateway_agent(
                &agent_id,
                true,
                Some("http://127.0.0.1:3425"),
                &["deepseek/deepseek-chat".to_string()],
            )
            .unwrap()
            .expect("a gateway profile");
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
            .set_gateway_agent(
                &agent_id,
                true,
                Some("http://127.0.0.1:3426"),
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

    fn profile_id(service: &ProviderConfigService) -> ModelProviderProfileId {
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
                agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                make_default: false,
            })
            .unwrap_err();
        assert_eq!(error.code, "agent_model_provider_binding_same_agent");
        let _ = std::fs::remove_file(path);
    }
}
