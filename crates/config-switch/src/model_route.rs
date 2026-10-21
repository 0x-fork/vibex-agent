//! Route management on the Provider configuration service.
//!
//! The repository owns the rows; this layer owns the rules that span them: a
//! route may only name a model its Provider actually serves, a route list is
//! bounded, and binding a route to an Agent must leave that Agent with at most
//! one default.
//!
//! Keeping these here rather than in the UI means the same checks apply to the
//! Config Center, the remote client, and anything added later.
//!
//! # Which layer to call
//!
//! The read methods mirror the repository on purpose, because every other
//! Provider surface does the same: a client never opens a database connection
//! for a Config Center page. The distinction that matters is *writes*.
//!
//! - **Reads** may go through either layer; this one is the one clients use,
//!   because it does not hand them a `Connection`.
//! - **Writes must go through this layer.** `create_model_route` stamps the
//!   revision and checks the member models exist; `update_model_route` sorts and
//!   validates; `bind_model_route` refuses a disabled route and keeps exactly
//!   one default. Calling `ModelRouteRepository` directly skips all of it — the
//!   only reason that repository is public is so migrations can seed rows
//!   without going through a service that is still being constructed.
//!
//! A read that is genuinely a repository query with no rule attached (say,
//! listing routes for a migration) may call the repository; a read under a rule
//! belongs here next to the rule.

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
    fn updating_a_route_keeps_the_order_it_was_given() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a", "b"]);
        let route = service
            .create_model_route(gateway_route(deepseek, &["a", "b"]))
            .unwrap();

        let mut next = route.clone();
        next.members.reverse();
        for (index, member) in next.members.iter_mut().enumerate() {
            member.order_index = index as i64;
        }
        let updated = service.update_model_route(next, route.revision).unwrap();
        assert_eq!(updated.revision, route.revision + 1);
        assert_eq!(
            updated
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn updating_to_an_unserved_model_is_refused_and_the_route_is_untouched() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service
            .create_model_route(gateway_route(deepseek.clone(), &["a"]))
            .unwrap();

        let mut next = route.clone();
        next.members[0].model_id = "not-a-model".to_string();
        let error = service
            .update_model_route(next, route.revision)
            .unwrap_err();
        assert_eq!(error.code, "model_route_member_model_unknown");

        // The rejected write must not have landed.
        let stored = service.get_model_route(&route.id).unwrap().unwrap();
        assert_eq!(stored.members[0].model_id, "a");
        assert_eq!(stored.revision, route.revision);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn deleting_a_route_unbinds_it_and_removes_it_from_the_list() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service
            .create_model_route(gateway_route(deepseek, &["a"]))
            .unwrap();
        let agent = AgentId::parse("codex").unwrap();
        service.bind_model_route(&agent, &route.id, true).unwrap();

        assert!(service.delete_model_route(&route.id).unwrap());
        assert!(service.get_model_route(&route.id).unwrap().is_none());
        assert!(service.list_model_routes().unwrap().is_empty());
        assert!(
            service.list_agent_model_routes(&agent).unwrap().is_empty(),
            "a deleted route must not stay bound"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unbinding_removes_only_that_agents_binding() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service
            .create_model_route(gateway_route(deepseek, &["a"]))
            .unwrap();
        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();
        service.bind_model_route(&codex, &route.id, true).unwrap();
        service.bind_model_route(&claude, &route.id, true).unwrap();

        assert!(service.unbind_model_route(&codex, &route.id).unwrap());
        assert!(service.list_agent_model_routes(&codex).unwrap().is_empty());
        assert_eq!(
            service.list_agent_model_routes(&claude).unwrap().len(),
            1,
            "the other Agent keeps its binding"
        );
        assert!(
            !service.unbind_model_route(&codex, &route.id).unwrap(),
            "unbinding twice reports that there was nothing left to remove"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn one_route_can_serve_two_agents_and_reports_both() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service
            .create_model_route(gateway_route(deepseek, &["a"]))
            .unwrap();
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            service.bind_model_route(&agent, &route.id, false).unwrap();
        }

        let users = service.list_agent_model_route_users(&route.id).unwrap();
        assert_eq!(
            users
                .iter()
                .map(|binding| binding.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude", "codex"]
        );
        let _ = std::fs::remove_file(path);
    }

    /// The stage-1 acceptance criterion, end to end.
    ///
    /// Two routes with different member orders, two Agents bound to different
    /// ones, and both read back exactly as written. Nothing here goes through
    /// the repository, so this is also the test that would catch the service
    /// and the repository drifting apart.
    #[test]
    fn two_routes_bound_to_two_agents_read_back_in_their_own_order() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["ds-chat", "ds-reasoner"]);
        let kimi = provider(&service, "Kimi", &["kimi-k3"]);

        // Route A: DeepSeek first, Kimi second.
        let mut route_a = gateway_route(deepseek.clone(), &["ds-chat", "ds-reasoner"]);
        route_a.display_name = "Standard".to_string();
        route_a
            .members
            .push(ModelRouteMember::new(kimi.clone(), "kimi-k3", 2));
        let route_a = service.create_model_route(route_a).unwrap();

        // Route B: Kimi first, DeepSeek second.
        let mut route_b = gateway_route(kimi.clone(), &["kimi-k3"]);
        route_b.display_name = "Kimi first".to_string();
        route_b
            .members
            .push(ModelRouteMember::new(deepseek.clone(), "ds-reasoner", 1));
        let route_b = service.create_model_route(route_b).unwrap();

        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();
        service.bind_model_route(&codex, &route_a.id, true).unwrap();
        service
            .bind_model_route(&claude, &route_b.id, true)
            .unwrap();

        let codex_bindings = service.list_agent_model_routes(&codex).unwrap();
        assert_eq!(codex_bindings.len(), 1);
        assert_eq!(codex_bindings[0].model_route_id, route_a.id);
        assert!(codex_bindings[0].is_default);

        let claude_bindings = service.list_agent_model_routes(&claude).unwrap();
        assert_eq!(claude_bindings[0].model_route_id, route_b.id);

        let read_a = service.get_model_route(&route_a.id).unwrap().unwrap();
        assert_eq!(
            read_a
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ds-chat", "ds-reasoner", "kimi-k3"]
        );
        let read_b = service.get_model_route(&route_b.id).unwrap().unwrap();
        assert_eq!(
            read_b
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["kimi-k3", "ds-reasoner"],
            "the two routes keep independent orders over the same Providers"
        );

        // One route is one model id, whatever its members are.
        assert_ne!(read_a.model_id(), read_b.model_id());
        assert!(read_a.model_id().starts_with("route/"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_route_may_hold_at_most_sixteen_members() {
        let (path, service) = service();
        let models = (0..17)
            .map(|index| format!("model-{index}"))
            .collect::<Vec<_>>();
        let refs = models.iter().map(String::as_str).collect::<Vec<_>>();
        let deepseek = provider(&service, "DeepSeek", &refs);

        let at_limit = gateway_route(deepseek.clone(), &refs[..16]);
        assert!(service.create_model_route(at_limit).is_ok());

        let mut over = gateway_route(deepseek, &refs[..17]);
        over.display_name = "Too many".to_string();
        let error = service.create_model_route(over).unwrap_err();
        assert_eq!(error.code, "route_member_limit_exceeded");
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

#[cfg(test)]
mod catalog_tests {
    use super::super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileId, ModelProviderProfileStatus, ModelRoute,
        ModelRouteMember, RouteKind, RouteStatus, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-catalog-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn provider(
        service: &ProviderConfigService,
        name: &str,
        models: &[&str],
    ) -> ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: name.to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
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
                default_model_id: models.first().map(|model| (*model).to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// The catalog lists one entry per route, and an entry's id carries no
    /// Provider prefix: the choice of Provider belongs to the route.
    #[test]
    fn the_catalog_lists_one_entry_per_route_without_a_provider_name() {
        let (path, service) = service();
        let first = provider(&service, "DeepSeek", &["flash"]);
        let second = provider(&service, "OpenRouter", &["flash"]);
        service
            .create_model_route(ModelRoute {
                display_name: "Standard".to_string(),
                kind: RouteKind::Gateway,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![
                    ModelRouteMember::new(first.clone(), "flash", 0),
                    ModelRouteMember::new(second.clone(), "flash", 1),
                ],
                ..Default::default()
            })
            .unwrap();

        let catalog = service.gateway_model_catalog().unwrap();
        let entry = catalog
            .iter()
            .find(|entry| entry.display_name == "Standard")
            .expect("the route is listed");
        assert_eq!(entry.source_count, 2, "two sources back it");
        assert_eq!(entry.model_count, 1, "but only one distinct model name");
        assert!(
            entry.display_id().starts_with("route/"),
            "a route id is not a Provider-qualified model: {}",
            entry.display_id()
        );
        assert!(
            !entry.display_id().contains("DeepSeek") && !entry.display_id().contains("OpenRouter"),
            "the id must not name either Provider: {}",
            entry.display_id()
        );
        let _ = std::fs::remove_file(path);
    }

    /// A disabled route is not offered, and a route whose Providers are gone is
    /// dropped rather than listed as a model nothing can serve.
    #[test]
    fn a_disabled_route_is_not_listed() {
        let (path, service) = service();
        let provider_id = provider(&service, "DeepSeek", &["flash"]);
        service
            .create_model_route(ModelRoute {
                display_name: "Off".to_string(),
                kind: RouteKind::Gateway,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Disabled,
                members: vec![ModelRouteMember::new(provider_id, "flash", 0)],
                ..Default::default()
            })
            .unwrap();

        let catalog = service.gateway_model_catalog().unwrap();
        assert!(
            !catalog.iter().any(|entry| entry.display_name == "Off"),
            "a disabled route must not be offered"
        );
        let _ = std::fs::remove_file(path);
    }
}
