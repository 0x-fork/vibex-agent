use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    AgentAuthContextId, AgentAuthenticationOperationId, AgentId, ProviderProfileId,
    SessionConfigValue, SessionRuntimeFeature, TerminalAuthActionDescriptor, VibexSessionId,
};

/// Product-level ACP authentication method. Protocol-specific payloads stay in
/// the ACP adapter; settings surfaces receive only safe fields they can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthMethodKind {
    Agent,
    Environment,
    Terminal,
}

/// Describes where a successful authentication action leaves reusable
/// credentials. A method is an action, never a durable runtime identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthMethodEffect {
    WritesAgentStateHome,
    RequiresProviderProfile,
    AgentManagedExternal,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthEnvironmentVariable {
    pub name: String,
    pub label: Option<String>,
    pub secret: bool,
    pub optional: bool,
    /// Whether the selected Provider Profile already owns a usable value for
    /// this exact environment key. The value itself never crosses this API.
    #[serde(default)]
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: AgentAuthMethodKind,
    #[serde(default)]
    pub effect: AgentAuthMethodEffect,
    pub environment: Vec<AgentAuthEnvironmentVariable>,
    pub credential_link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthContextStatus {
    Unverified,
    Verifying,
    AuthenticationRequired,
    Authenticated,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContext {
    pub id: AgentAuthContextId,
    pub agent_id: AgentId,
    pub status: AgentAuthContextStatus,
    pub account_hint: Option<String>,
    pub authenticated_via_method: Option<String>,
    pub revision: i64,
    pub last_verified_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthenticationOperationState {
    Queued,
    DiscoveringMethods,
    Authenticating,
    AwaitingUser,
    Verifying,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthExecutionLocation {
    HostBrowser,
    ClientBrowserWithDeviceCode,
    HostTerminal,
    RemoteAttachableTerminal,
    #[default]
    CompletedOnHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthenticationOperation {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    pub method_id: String,
    pub state: AgentAuthenticationOperationState,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelDiscoverySource {
    DirectCatalog,
    SessionConfig,
    CompatibilityDescriptor,
    LiveSession,
    AgentDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthModelCatalogStatus {
    Unknown,
    Discovering,
    Available,
    AgentDefaultOnly,
    AuthenticationRequired,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthModelDescriptor {
    pub model_id: String,
    /// How the Agent names this Model.
    ///
    /// It is the Agent's own wording, which is what carries the account a
    /// Model is billed to when one id is served through more than one gateway;
    /// the model id stands in only when the Agent published no name.
    pub label: String,
    #[serde(default)]
    pub reasoning_efforts: Vec<SessionConfigValue>,
    #[serde(default)]
    pub modes: Vec<SessionConfigValue>,
    #[serde(default)]
    pub features: Vec<SessionRuntimeFeature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthModelCatalogSnapshot {
    pub auth_context_id: AgentAuthContextId,
    pub auth_context_revision: i64,
    pub runtime_fingerprint: String,
    pub discovery_source: AgentModelDiscoverySource,
    pub status: AgentAuthModelCatalogStatus,
    pub models: Vec<AgentAuthModelDescriptor>,
    /// Whether every published model/default entry has model-scoped runtime
    /// controls. Older snapshots deserialize as incomplete and are refreshed.
    #[serde(default)]
    pub runtime_options_complete: bool,
    /// Whether the discovery that built this snapshot kept the names the Agent
    /// publishes for its Models.
    ///
    /// Older snapshots deserialize as incomplete and are refreshed as well: a
    /// catalog that replaced every name with its model id cannot say which
    /// account or plan a Model belongs to when one id is served by several.
    #[serde(default)]
    pub model_names_complete: bool,
    /// Runtime controls advertised for the semantic Agent-default model when
    /// the Agent does not enumerate concrete model ids.
    #[serde(default)]
    pub default_reasoning_efforts: Vec<SessionConfigValue>,
    #[serde(default)]
    pub default_modes: Vec<SessionConfigValue>,
    #[serde(default)]
    pub default_features: Vec<SessionRuntimeFeature>,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

/// CAS-fenced request to authenticate the one default account owned by an
/// Agent. `method_id` selects an action only; it never becomes runtime identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextAuthenticateRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    pub method_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextVerifyRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    /// Present when verification completes an interactive authentication
    /// operation that previously returned an auth terminal.
    pub operation_id: Option<AgentAuthenticationOperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextCancelAuthenticationRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextRefreshModelsRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextLogoutRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    /// The caller must echo the current impact count after presenting it to
    /// the user. A concurrent session change therefore forces a new preview.
    pub confirmed_affected_session_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextLogoutPreview {
    pub context: AgentAuthContext,
    pub affected_session_ids: Vec<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextMutationResult {
    pub context: AgentAuthContext,
    pub model_catalog: Option<AgentAuthModelCatalogSnapshot>,
    #[serde(default)]
    pub affected_session_ids: Vec<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextAuthenticateResult {
    pub context: AgentAuthContext,
    pub operation: AgentAuthenticationOperation,
    #[serde(default)]
    pub execution_location: AgentAuthExecutionLocation,
    pub terminal: Option<TerminalAuthActionDescriptor>,
    pub model_catalog: Option<AgentAuthModelCatalogSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthStatus {
    Unknown,
    AuthenticationRequired,
    Authenticated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthCatalog {
    pub agent_id: AgentId,
    pub methods: Vec<AgentAuthMethod>,
    pub supports_logout: bool,
    pub status: AgentAuthStatus,
    pub refreshed_at_ms: i64,
}

impl AgentAuthCatalog {
    /// Whether any advertised method can carry a credential change this client
    /// can actually drive. See [`AgentAuthMethod::is_client_drivable`].
    pub fn supports_reauthentication(&self) -> bool {
        self.methods
            .iter()
            .any(|method| method.is_client_drivable(&self.agent_id, self.supports_logout))
    }

    /// Whether the Agent advertises sign-in methods but none this client can
    /// act on, so its credentials live entirely outside Vibex.
    ///
    /// This gates an explanation, not an action: such an Agent still deserves an
    /// honest statement rather than a button that cannot do anything.
    pub fn is_externally_managed(&self) -> bool {
        !self.supports_reauthentication() && self.methods.iter().any(AgentAuthMethod::is_visible)
    }
}

impl AgentAuthMethod {
    /// Whether an Agent-account surface may list this method at all.
    ///
    /// `Environment` methods belong to a Provider Profile and are edited through
    /// the profile's credential fields, so they are not part of Agent sign-in.
    pub fn is_visible(&self) -> bool {
        self.effect != AgentAuthMethodEffect::RequiresProviderProfile
            && self.kind != AgentAuthMethodKind::Environment
    }

    /// Whether this method can carry a credential change the client can drive.
    ///
    /// A method kind is not evidence that the Agent will do anything. `Agent`
    /// kind only means the Agent handles authentication itself, and several
    /// Agents answer `authenticate` with an empty result and no side effect —
    /// `glm-acp-agent` documents exactly that, configuring its credential out of
    /// band instead. Offering "re-authenticate" for such a method runs a no-op
    /// and still reports success, which is worse than offering nothing.
    ///
    /// A method is drivable when the client itself can carry the sign-in:
    ///
    /// - `Terminal`: the catalog holds a login command, so the client opens a
    ///   terminal. This also covers Agents whose login is declared as an `Agent`
    ///   method with the reserved `_meta.terminal-auth` descriptor, because the
    ///   parser upgrades that shape to `Terminal` on the way in.
    /// - `supports_logout`: the Agent advertises a credential store it can
    ///   rewrite. Codex is the reference case — its `chat-gpt` method is `Agent`
    ///   kind with no terminal descriptor, but the adapter opens the browser
    ///   itself and blocks until the OAuth round trip finishes.
    /// - A verified host-browser pair: the Agent answers `authenticate` by
    ///   starting a browser flow. Cursor, Grok's `grok.com`, Gemini's
    ///   `oauth-personal`, Cline, CodeBuddy and Antigravity's OAuth methods are
    ///   the observed cases. They advertise no `logout`, so `supports_logout`
    ///   alone would hide a sign-in that genuinely works.
    ///
    /// `Environment` methods are excluded: they belong to a Provider Profile and
    /// are edited through the profile's credential fields, not through Agent
    /// sign-in.
    pub fn is_client_drivable(&self, agent_id: &AgentId, supports_logout: bool) -> bool {
        match self.kind {
            AgentAuthMethodKind::Terminal => true,
            AgentAuthMethodKind::Environment => false,
            AgentAuthMethodKind::Agent => {
                supports_logout
                    || crate::acp_agent_method_opens_host_browser(agent_id.as_str(), &self.id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticateRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub method_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticationCancelRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub agent_id: AgentId,
}

/// Releases the adapter-side ownership for an interactive authentication
/// operation after its terminal has reached a final exit state. This is
/// separate from cancellation because a successful terminal must remain
/// available for verification and final-output inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticationCompleteRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticateResult {
    pub method_id: String,
    pub terminal: Option<TerminalAuthActionDescriptor>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentAuthEnvironmentValue {
    pub name: String,
    pub value: Option<String>,
    pub secret: bool,
    pub optional: bool,
    /// Explicit removal intent. A blank value without this flag preserves an
    /// existing credential reference, which keeps masked settings forms safe.
    pub clear: bool,
}

impl fmt::Debug for AgentAuthEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAuthEnvironmentValue")
            .field("name", &self.name)
            .field(
                "has_value",
                &self.value.as_ref().is_some_and(|v| !v.is_empty()),
            )
            .field("secret", &self.secret)
            .field("optional", &self.optional)
            .field("clear", &self.clear)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentAuthEnvironmentUpdateRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub method_id: String,
    pub values: Vec<AgentAuthEnvironmentValue>,
}

impl fmt::Debug for AgentAuthEnvironmentUpdateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAuthEnvironmentUpdateRequest")
            .field("agent_id", &self.agent_id)
            .field("provider_profile_id", &self.provider_profile_id)
            .field("method_id", &self.method_id)
            .field("values", &self.values)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLogoutRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_auth_debug_never_contains_credential_values() {
        let value = AgentAuthEnvironmentValue {
            name: "EXAMPLE_API_KEY".to_string(),
            value: Some("secret-value".to_string()),
            secret: true,
            optional: false,
            clear: false,
        };
        let request = AgentAuthEnvironmentUpdateRequest {
            agent_id: AgentId::parse("example-agent").unwrap(),
            provider_profile_id: ProviderProfileId::new(),
            method_id: "api-key".to_string(),
            values: vec![value],
        };

        let debug = format!("{request:?}");
        assert!(debug.contains("EXAMPLE_API_KEY"));
        assert!(debug.contains("has_value: true"));
        assert!(!debug.contains("secret-value"));
    }

    fn agent_method(id: &str) -> AgentAuthMethod {
        AgentAuthMethod {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind: AgentAuthMethodKind::Agent,
            effect: AgentAuthMethodEffect::WritesAgentStateHome,
            environment: Vec::new(),
            credential_link: None,
        }
    }

    fn catalog(
        agent: &str,
        methods: Vec<AgentAuthMethod>,
        supports_logout: bool,
    ) -> AgentAuthCatalog {
        AgentAuthCatalog {
            agent_id: AgentId::parse(agent).unwrap(),
            methods,
            supports_logout,
            status: AgentAuthStatus::Authenticated,
            refreshed_at_ms: 0,
        }
    }

    /// An `Agent` method is only a sign-in when the Agent's own `authenticate`
    /// actually starts one. Cursor, Grok's `grok.com`, Gemini's `oauth-personal`,
    /// Cline, CodeBuddy and Antigravity's OAuth methods open a browser but
    /// advertise no logout; treating "no logout" as "nothing to drive" hid a
    /// working sign-in and told the user the credential was managed elsewhere.
    #[test]
    fn host_browser_agents_are_drivable_without_advertising_logout() {
        for (agent, method) in [
            ("cursor", "cursor_login"),
            ("grok", "grok.com"),
            ("gemini", "oauth-personal"),
            ("cline", "cline"),
            ("codebuddy-code", "external"),
            ("antigravity", "oauth-personal"),
            ("devin", "devin-browser"),
        ] {
            let catalog = catalog(agent, vec![agent_method(method)], false);
            assert!(
                catalog.supports_reauthentication(),
                "{agent}/{method} opens a browser but was called undrivable"
            );
            assert!(
                !catalog.is_externally_managed(),
                "{agent}/{method} must not be explained as externally managed"
            );
        }
    }

    /// The no-op cases must keep failing closed, and the granularity is per
    /// method: inside one Agent, one method can open a browser while its
    /// sibling API-key method just reports a missing variable.
    #[test]
    fn methods_that_answer_without_a_sign_in_stay_external() {
        // zcode's bridge does not implement `authenticate` at all.
        let zcode = catalog("zcode", vec![agent_method("zcode-credentials")], false);
        assert!(!zcode.supports_reauthentication());
        assert!(zcode.is_externally_managed());

        // glm-acp-agent documents an empty result and configures out of band.
        let glm = catalog("glm-acp-agent", vec![agent_method("z-ai-api-key")], false);
        assert!(!glm.supports_reauthentication());
        assert!(glm.is_externally_managed());

        // Grok and Gemini have one browser method each; their API-key methods
        // fail immediately when the variable is unset, so they stay external on
        // their own.
        for (agent, method) in [
            ("grok", "xai.api_key"),
            ("gemini", "gemini-api-key"),
            ("gemini", "vertex-ai"),
            ("gemini", "gateway"),
            ("cursor", "cursor_api_key"),
            ("codex", "api-key"),
        ] {
            let method = agent_method(method);
            assert!(
                !method.is_client_drivable(&AgentId::parse(agent).unwrap(), false),
                "{agent}/{method:?} does not start a sign-in but was called drivable"
            );
        }
    }

    /// An Agent that advertises ACP `logout` owns a credential store it can
    /// rewrite, which makes its `Agent` methods drivable without any per-Agent
    /// evidence — Codex is the reference case.
    #[test]
    fn advertised_logout_makes_agent_methods_drivable() {
        let codex = catalog("codex", vec![agent_method("chat-gpt")], true);
        assert!(codex.supports_reauthentication());
        assert!(!codex.is_externally_managed());
    }

    /// A terminal method is always drivable: the client attaches to the PTY the
    /// authority opened, whatever the logout capability says.
    #[test]
    fn terminal_methods_are_drivable_and_environment_methods_are_not() {
        let terminal = AgentAuthMethod {
            kind: AgentAuthMethodKind::Terminal,
            ..agent_method("vibex-cli-login")
        };
        let catalog = catalog("kiro", vec![terminal.clone()], false);
        assert!(catalog.supports_reauthentication());
        assert!(terminal.is_visible());
        assert!(terminal.is_client_drivable(&AgentId::parse("kiro").unwrap(), false));

        let environment = AgentAuthMethod {
            kind: AgentAuthMethodKind::Environment,
            effect: AgentAuthMethodEffect::RequiresProviderProfile,
            ..agent_method("shared-profile")
        };
        assert!(!environment.is_visible());
        assert!(!environment.is_client_drivable(&AgentId::parse("kiro").unwrap(), true));
    }

    /// The verified table and the drivability rule cannot drift: every pair the
    /// table lists must be Agent-kind (a terminal change would make the entry a
    /// lie) and must resolve to a real catalog Agent.
    #[test]
    fn verified_browser_pairs_name_real_agents_and_agent_kind_methods() {
        let agents = crate::acp_agent_catalog_entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<std::collections::BTreeSet<_>>();
        for (agent, method) in crate::ACP_AGENT_HOST_BROWSER_LOGIN_METHODS {
            assert!(
                agents.contains(agent),
                "{agent} has a verified browser method but is not a catalog Agent"
            );
            assert!(!method.is_empty(), "{agent} lists an empty method id");
            assert!(
                crate::acp_agent_method_opens_host_browser(agent, method),
                "{agent}/{method} was listed but does not resolve"
            );
        }
        assert!(!crate::acp_agent_method_opens_host_browser(
            "grok",
            "xai.api_key"
        ));
    }
}
