//! Route management on the Provider configuration service.
//!
//! The repository owns the rows; this layer owns the rules that span them: a
//! route may only name a model its Provider actually serves, a route list is
//! bounded, and binding a route to an Agent must leave that Agent with at most
//! one default.
//!
//! Keeping these here rather than in the UI means the same checks apply to the
//! Config Center, the remote client, and anything added later.

use vibex_core::{
    AgentId, AgentModelRouteBinding, MAX_ROUTES, ModelProviderProfile, ModelRoute, ModelRouteId,
    RouteStatus, VibexError, VibexResult,
};
use vibex_db::{
    AgentModelRouteBindingRepository, ModelProviderProfileRepository, ModelRouteRepository,
};

use crate::ProviderConfigService;

impl ProviderConfigService {
    /// Every live route, most recently changed first.
    pub fn list_model_routes(&self) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list(&conn)
    }

    /// Every live, enabled route.
    pub fn list_enabled_model_routes(&self) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list_enabled(&conn)
    }

    pub fn get_model_route(&self, id: &ModelRouteId) -> VibexResult<Option<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::get(&conn, id)
    }

    /// Every route that names this Provider.
    ///
    /// The delete and edit paths read this first, so a Provider change can name
    /// what it would break instead of failing inside a projection later.
    pub fn list_model_routes_naming_provider(
        &self,
        provider: &vibex_core::ModelProviderProfileId,
    ) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list_naming_provider(&conn, provider)
    }

    /// Every route bound to this Agent, default first.
    pub fn list_agent_model_routes(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)
    }

    /// The Agents that reference this route.
    pub fn list_agent_model_route_users(
        &self,
        route_id: &ModelRouteId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::list_for_route(&conn, route_id)
    }

    pub fn create_model_route(&self, mut route: ModelRoute) -> VibexResult<ModelRoute> {
        let now = vibex_core::unix_timestamp_ms().max(1);
        route.revision = 1;
        route.created_at_ms = now;
        route.updated_at_ms = now;
        route.deleted_at_ms = None;
        route.sort_members();
        route.validate()?;
        let conn = self.open_connection()?;
        if ModelRouteRepository::list(&conn)?.len() >= MAX_ROUTES {
            return Err(VibexError::validation(
                "model_route_limit_exceeded",
                "there are already as many routes as this build supports",
            ));
        }
        self.require_members_are_served(&conn, &route)?;
        ModelRouteRepository::insert(&conn, &route)?;
        self.get_model_route(&route.id)?.ok_or_else(|| {
            VibexError::storage(
                "model_route_create_readback_failed",
                "failed to read the model route after create",
            )
        })
    }

    /// Replaces a route, including its member order.
    pub fn update_model_route(
        &self,
        mut route: ModelRoute,
        expected_revision: i64,
    ) -> VibexResult<ModelRoute> {
        route.sort_members();
        route.validate()?;
        let conn = self.open_connection()?;
        self.require_members_are_served(&conn, &route)?;
        ModelRouteRepository::update(&conn, &route, expected_revision)
    }

    /// Deletes a route and every binding that pointed at it.
    pub fn delete_model_route(&self, id: &ModelRouteId) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        ModelRouteRepository::delete(&conn, id)
    }

    /// Binds a route to an Agent, optionally making it the Agent's default.
    pub fn bind_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
        make_default: bool,
    ) -> VibexResult<AgentModelRouteBinding> {
        let conn = self.open_connection()?;
        let route = ModelRouteRepository::get(&conn, route_id)?.ok_or_else(|| {
            VibexError::validation("model_route_not_found", "the route was not found")
                .with_diagnostic("modelRouteId", route_id.as_str())
        })?;
        if route.status != RouteStatus::Enabled {
            return Err(VibexError::validation(
                "model_route_disabled",
                "a disabled route cannot be bound to an Agent",
            )
            .with_diagnostic("modelRouteId", route_id.as_str()));
        }
        let existing = AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?
            .into_iter()
            .find(|binding| &binding.model_route_id == route_id);
        if let Some(binding) = existing {
            // Binding twice is a no-op rather than a duplicate row; only the
            // default may still need to move.
            if make_default && !binding.is_default {
                AgentModelRouteBindingRepository::set_default(&conn, agent_id, route_id)?;
            }
            return Ok(
                AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?
                    .into_iter()
                    .find(|binding| &binding.model_route_id == route_id)
                    .unwrap_or(binding),
            );
        }
        let now = vibex_core::unix_timestamp_ms().max(1);
        let mut binding = AgentModelRouteBinding::new(agent_id.clone(), route_id.clone());
        binding.is_default = make_default;
        binding.created_at_ms = now;
        binding.updated_at_ms = now;
        AgentModelRouteBindingRepository::insert(&conn, &binding)?;
        Ok(binding)
    }

    pub fn set_agent_default_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<()> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::set_default(&conn, agent_id, route_id)
    }

    pub fn unbind_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::delete(&conn, agent_id, route_id)
    }

    /// Refuses a member that names a model its Provider does not serve.
    ///
    /// This is the check a typo meets first. Without it the mistake surfaces as
    /// a 404 from the gateway at request time, long after the route was saved
    /// and attributed to a Provider that never had the model.
    fn require_members_are_served(
        &self,
        conn: &vibex_db::DbConnection,
        route: &ModelRoute,
    ) -> VibexResult<()> {
        let profiles = ModelProviderProfileRepository::list(conn)?;
        for member in &route.members {
            let profile = profiles
                .iter()
                .find(|profile| profile.id == member.model_provider_profile_id)
                .ok_or_else(|| {
                    VibexError::validation(
                        "model_route_member_provider_missing",
                        "a route member names a Provider that does not exist",
                    )
                    .with_diagnostic(
                        "modelProviderProfileId",
                        member.model_provider_profile_id.as_str(),
                    )
                })?;
            if !model_is_served(profile, &member.model_id) {
                return Err(VibexError::validation(
                    "model_route_member_model_unknown",
                    "a route member names a model its Provider does not serve",
                )
                .with_diagnostic("modelProviderProfileId", profile.id.as_str())
                .with_diagnostic("modelId", member.model_id.trim()));
            }
        }
        Ok(())
    }
}

/// Whether a Provider's catalogue lists this model.
///
/// A Provider with no configured models is treated as open: it may serve
/// anything its endpoint accepts, which is the same assumption the gateway
/// already makes when it forwards a model id it was not told about. Refusing
/// here would block a route against a Provider whose catalogue has not been
/// fetched yet.
fn model_is_served(profile: &ModelProviderProfile, model_id: &str) -> bool {
    let model_id = model_id.trim();
    if profile.configured_models.is_empty() {
        return !model_id.is_empty();
    }
    profile
        .configured_models
        .iter()
        .any(|entry| entry.enabled && entry.id == model_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileId, ModelProviderProfileStatus,
        ModelRouteMember, RouteKind, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-service-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let _ = std::fs::remove_file(&path);
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn provider(
        service: &ProviderConfigService,
        name: &str,
        models: &[&str],
    ) -> ModelProviderProfileId {
        let profile = ModelProviderProfile {
            display_name: name.to_string(),
            status: ModelProviderProfileStatus::Enabled,
            configured_models: models
                .iter()
                .map(|model| ModelProviderCatalogEntry {
                    id: (*model).to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                })
                .collect(),
            ..Default::default()
        };
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: profile.display_name.clone(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: profile.configured_models.clone(),
                default_model_id: models.first().map(|model| (*model).to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    fn gateway_route(provider: ModelProviderProfileId, models: &[&str]) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            kind: RouteKind::Gateway,
            strategy: RouteStrategy::Ordered,
            status: RouteStatus::Enabled,
            members: models
                .iter()
                .enumerate()
                .map(|(index, model)| ModelRouteMember::new(provider.clone(), *model, index as i64))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn creating_a_route_stamps_it_and_returns_the_stored_shape() {
        let (path, service) = service();
        let deepseek = provider(
            &service,
            "DeepSeek",
            &["deepseek-chat", "deepseek-reasoner"],
        );
        let mut route = gateway_route(deepseek, &["deepseek-chat"]);
        route.revision = 99; // the service owns the revision
        let created = service.create_model_route(route).unwrap();
        assert_eq!(created.revision, 1);
        assert!(created.created_at_ms > 0);
        assert_eq!(created.members.len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_member_naming_an_unserved_model_is_refused() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let route = gateway_route(deepseek, &["deepseek-chat", "not-a-model"]);
        let error = service.create_model_route(route).unwrap_err();
        assert_eq!(error.code, "model_route_member_model_unknown");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_member_naming_a_missing_provider_is_refused() {
        let (path, service) = service();
        let route = gateway_route(ModelProviderProfileId::new(), &["whatever"]);
        let error = service.create_model_route(route).unwrap_err();
        assert_eq!(error.code, "model_route_member_provider_missing");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_provider_with_no_catalogue_does_not_block_the_route() {
        let (path, service) = service();
        // No configured models: the catalogue has not been fetched yet. The
        // route must still be saveable, or the user could never compose one.
        let open = provider(&service, "Open", &[]);
        let route = gateway_route(open, &["anything-goes"]);
        assert!(service.create_model_route(route).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn binding_twice_is_a_no_op_and_the_default_can_still_move() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let first = service
            .create_model_route(gateway_route(deepseek.clone(), &["a"]))
            .unwrap();
        let mut second_route = gateway_route(deepseek, &["a"]);
        second_route.display_name = "Second".to_string();
        let second = service.create_model_route(second_route).unwrap();
        let agent = AgentId::parse("codex").unwrap();

        let bound = service.bind_model_route(&agent, &first.id, true).unwrap();
        assert!(bound.is_default);
        // Binding the same route again must not create a second row.
        let again = service.bind_model_route(&agent, &first.id, false).unwrap();
        assert!(again.is_default, "the existing default is left alone");

        service.bind_model_route(&agent, &second.id, false).unwrap();
        assert_eq!(service.list_agent_model_routes(&agent).unwrap().len(), 2);

        service
            .set_agent_default_model_route(&agent, &second.id)
            .unwrap();
        let bindings = service.list_agent_model_routes(&agent).unwrap();
        assert_eq!(bindings[0].model_route_id, second.id);
        assert!(bindings[0].is_default);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_disabled_route_cannot_be_bound() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let mut route = gateway_route(deepseek, &["a"]);
        route.status = RouteStatus::Disabled;
        let created = service.create_model_route(route).unwrap();
        let agent = AgentId::parse("claude").unwrap();
        let error = service
            .bind_model_route(&agent, &created.id, false)
            .unwrap_err();
        assert_eq!(error.code, "model_route_disabled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_provider_lookup_reports_the_routes_it_would_break() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let kimi = provider(&service, "Kimi", &["k3"]);
        service
            .create_model_route(gateway_route(deepseek.clone(), &["a"]))
            .unwrap();
        let mut mixed = gateway_route(deepseek.clone(), &["a"]);
        mixed
            .members
            .push(ModelRouteMember::new(kimi.clone(), "k3", 1));
        mixed.display_name = "Mixed".to_string();
        service.create_model_route(mixed).unwrap();

        assert_eq!(
            service
                .list_model_routes_naming_provider(&deepseek)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            service
                .list_model_routes_naming_provider(&kimi)
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_file(path);
    }
}
