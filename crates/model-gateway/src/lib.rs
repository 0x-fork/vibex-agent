//! Local model gateway.
//!
//! The gateway is one loopback HTTP endpoint that serves the wire APIs an Agent
//! already speaks — OpenAI Chat Completions, OpenAI Responses, Anthropic
//! Messages, and Gemini `generateContent` — and forwards each call to whichever
//! configured upstream Provider serves the requested model.
//!
//! Its three responsibilities are deliberately separate:
//!
//! - [`wire`] converts between the four APIs through one intermediate
//!   representation. A request whose protocol the upstream already speaks is
//!   forwarded byte for byte; translation only happens when it has to.
//! - [`route`] chooses an upstream and one of its Keys, applies cooling-down
//!   after a failure, and keeps a conversation on the Key that holds its prompt
//!   cache.
//! - [`server`] owns the HTTP surface and [`ModelGatewayService`] owns the
//!   listener lifecycle, including port persistence.
//!
//! Routing state is request state. Nothing here is part of an Agent process
//! fingerprint, so replacing a Key never restarts an Agent.

mod error;
mod ir;
mod limit;
mod route;
mod server;
mod service;
pub mod stream;
mod tool_names;
mod wire;

pub use error::GatewayError;
pub use ir::{
    CanonicalEvent, CanonicalMessage, CanonicalRequest, CanonicalResponse, CanonicalUsage,
    ContentPart, ImageSource, OpaqueSignature, ReasoningRequest, Role, SamplingParams, StopReason,
    ToolChoice, ToolDefinition,
};
pub use limit::{Limiter, Permit};
pub use route::{
    CandidateKey, KeyHealth, ResolvedModel, RoutePlan, RouteRequest, classify_failure,
    conversation_key, plan_route, resolve_model,
};
pub use server::{GatewayRouterState, build_router};
pub use service::{
    GatewayKeySummary, GatewayLimits, GatewayModelEntry, GatewayRuntime, GatewaySnapshot,
    GatewayUpstream, ModelGatewayService, PreparedCall,
};
pub use wire::{WireProtocol, build_upstream_request, parse_reply, parse_request, render_response};
