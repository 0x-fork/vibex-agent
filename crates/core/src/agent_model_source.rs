//! The one default model source an Agent uses, and how it is addressed.
//!
//! A Provider default and a route default used to be two independent pieces of
//! state: `agent_default_model_provider_profiles` said which Provider an Agent
//! starts with, while `agent_model_route_bindings.is_default` said which route
//! it starts with. Both could be set at once, and nothing decided which one a
//! session should follow. This module adds the unified record the read path and
//! the next wave's writer share.
//!
//! The record keeps the scope the legacy Provider default already had. Routes
//! are Assets with no Agent or scope of their own, so a route default is stored
//! in every scope the reader asks about; `scope` here says *where the answer was
//! asked for*, not that two scopes share the route.
//!
//! `source` is `Option` because a read is a resolution, not a row lookup: when
//! no unified row exists the service answers from the legacy fields, and when
//! none of those apply either the honest answer is "no default", not an error.

use serde::{Deserialize, Serialize};

use crate::agent_config::AgentId;
use crate::ids::{ModelRouteId, ProviderProfileId};
use crate::provider::ProviderProfileDefaultScope;

/// A source an Agent may default to: one shared Provider or one route group.
///
/// The wire shape is internally tagged (`{"kind":"provider","id":"provider_…"}`)
/// because the two ids cannot be told apart by shape, and a reader that sees
/// only `id` would have to guess. The tag names the variant, the content names
/// the id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AgentModelSourceRef {
    Provider(ProviderProfileId),
    RouteGroup(ModelRouteId),
}

impl AgentModelSourceRef {
    pub fn provider(provider_profile_id: ProviderProfileId) -> Self {
        Self::Provider(provider_profile_id)
    }

    pub fn route_group(model_route_id: ModelRouteId) -> Self {
        Self::RouteGroup(model_route_id)
    }

    pub fn is_provider(&self) -> bool {
        matches!(self, Self::Provider(_))
    }

    pub fn is_route_group(&self) -> bool {
        matches!(self, Self::RouteGroup(_))
    }

    pub fn provider_profile_id(&self) -> Option<&ProviderProfileId> {
        match self {
            Self::Provider(provider_profile_id) => Some(provider_profile_id),
            Self::RouteGroup(_) => None,
        }
    }

    pub fn model_route_id(&self) -> Option<&ModelRouteId> {
        match self {
            Self::Provider(_) => None,
            Self::RouteGroup(model_route_id) => Some(model_route_id),
        }
    }
}

/// Where an Agent's default model source was asked for.
///
/// `Global` is the ordinary Agent page. A workspace or project reader asks the
/// same question in the scope it is managing, which is why the scope part of
/// the primary key is retained after the two defaults were unified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelSourceDefault {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
    /// `None` means the Agent has no default source at all, not "the row is
    /// absent": a caller always receives one answer per Agent and scope.
    #[serde(default)]
    pub source: Option<AgentModelSourceRef>,
    /// When the answer last changed. A resolved legacy answer carries the
    /// timestamp of the row it resolved from; "no default" carries `0`.
    pub updated_at_ms: i64,
}

impl AgentModelSourceDefault {
    pub fn none(scope: ProviderProfileDefaultScope, agent_id: AgentId) -> Self {
        Self {
            scope,
            agent_id,
            source: None,
            updated_at_ms: 0,
        }
    }

    pub fn source(&self) -> Option<&AgentModelSourceRef> {
        self.source.as_ref()
    }
}

/// Reads the resolved default for one Agent in one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelSourceDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
}

/// Replaces the default with one Provider or route group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAgentModelSourceDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
    pub source: AgentModelSourceRef,
}

/// Removes the explicit unified default for one Agent in one scope.
///
/// Clearing does not remove the legacy Provider/route defaults the read path
/// falls back to; it removes the unified row that had been taking precedence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearAgentModelSourceDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderDefaultScopeKind;

    fn scope() -> ProviderProfileDefaultScope {
        ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        }
    }

    #[test]
    fn agent_model_source_provider_ref_round_trips_as_tagged_camel_case() {
        let source = AgentModelSourceRef::Provider(
            ProviderProfileId::parse("provider_test_agent_model_source").unwrap(),
        );
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "provider",
                "id": "provider_test_agent_model_source"
            })
        );
        assert_eq!(
            serde_json::from_value::<AgentModelSourceRef>(value).unwrap(),
            source
        );
    }

    #[test]
    fn agent_model_source_route_ref_round_trips_as_tagged_camel_case() {
        let source = AgentModelSourceRef::RouteGroup(
            ModelRouteId::parse("model_route_test_agent_model_source").unwrap(),
        );
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "route_group",
                "id": "model_route_test_agent_model_source"
            })
        );
        assert_eq!(
            serde_json::from_value::<AgentModelSourceRef>(value).unwrap(),
            source
        );
    }

    #[test]
    fn agent_model_source_default_is_camel_case_and_admits_no_source() {
        let default = AgentModelSourceDefault {
            scope: scope(),
            agent_id: AgentId::parse("claude").unwrap(),
            source: None,
            updated_at_ms: 42,
        };
        let value = serde_json::to_value(&default).unwrap();
        assert_eq!(value["agentId"], "claude");
        assert_eq!(value["updatedAtMs"], 42);
        assert_eq!(value["source"], serde_json::Value::Null);
        let decoded: AgentModelSourceDefault = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, default);
    }

    #[test]
    fn agent_model_source_requests_use_camel_case_fields() {
        let request = SetAgentModelSourceDefaultRequest {
            scope: scope(),
            agent_id: AgentId::parse("codex").unwrap(),
            source: AgentModelSourceRef::Provider(
                ProviderProfileId::parse("provider_test_request_source").unwrap(),
            ),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["agentId"], "codex");
        assert_eq!(value["source"]["kind"], "provider");
        assert_eq!(value["scope"]["kind"], "global");
        assert_eq!(
            serde_json::from_value::<SetAgentModelSourceDefaultRequest>(value).unwrap(),
            request
        );
    }
}
