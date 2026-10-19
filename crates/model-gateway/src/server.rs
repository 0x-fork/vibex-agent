//! The gateway's HTTP surface.
//!
//! Only loopback callers are served, and only with a loopback `Host` header: a
//! browser on the machine can otherwise be pointed at `127.0.0.1` by a hostile
//! page, and DNS rebinding would make that page same-origin with the gateway.
//! The request paths mirror the four vendor APIs exactly, so an Agent's own
//! base-URL setting is all that has to change.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::mpsc;

use crate::error::GatewayError;
use crate::service::ModelGatewayService;
use crate::wire::WireProtocol;

/// State the router carries.
#[derive(Clone)]
pub struct GatewayRouterState {
    pub service: Arc<ModelGatewayService>,
}

/// Builds the gateway router.
pub fn build_router(service: Arc<ModelGatewayService>) -> Router {
    let body_limit = service.max_body_bytes();
    let state = GatewayRouterState { service };
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/responses", post(responses))
        .route("/v1/messages", post(anthropic_messages))
        .route("/messages", post(anthropic_messages))
        // Gemini's model name is part of the path and is followed by an action
        // suffix, so the whole tail is captured and parsed here.
        .route("/v1beta/models/{*call}", post(gemini_call))
        .route("/models", get(list_models))
        .route("/v1/models", get(list_models))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

async fn chat_completions(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::ChatCompletions, headers, body).await
}

async fn responses(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::Responses, headers, body).await
}

async fn anthropic_messages(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::AnthropicMessages, headers, body).await
}

async fn gemini_call(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    // `/v1beta/models/<model>:generateContent` and its streaming twin.
    let path = uri.path();
    let Some(tail) = path.strip_prefix("/v1beta/models/") else {
        return not_found();
    };
    let Some((model, action)) = tail.rsplit_once(':') else {
        return not_found();
    };
    if model.is_empty() || !matches!(action, "generateContent" | "streamGenerateContent") {
        return not_found();
    }
    serve(state, WireProtocol::GeminiGenerateContent, headers, body).await
}

/// Refuses a caller that is not loopback, or a browser request.
fn perimeter(headers: &HeaderMap) -> Result<(), GatewayError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let host_name = host
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(host)
        .trim_matches(['[', ']']);
    if !matches!(host_name, "127.0.0.1" | "localhost" | "::1") {
        return Err(GatewayError::NotConfigured(
            "the gateway only accepts loopback requests".to_string(),
        ));
    }
    // A browser sends Origin on every cross-origin request and on same-origin
    // preflighted ones. An Agent CLI never does, so any Origin means a page is
    // driving the connection.
    if headers.contains_key(header::ORIGIN) {
        return Err(GatewayError::NotConfigured(
            "the gateway does not accept browser requests".to_string(),
        ));
    }
    Ok(())
}

async fn serve(
    state: GatewayRouterState,
    protocol: WireProtocol,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(error) = perimeter(&headers) {
        return state.service.error_response(protocol, &error);
    }
    let service = Arc::clone(&state.service);
    let prepared = match service.prepare(protocol, &headers, &body) {
        Ok(prepared) => prepared,
        Err(error) => {
            let outcome = matches!(
                error,
                GatewayError::AllKeysCooling { .. } | GatewayError::TokenCeilingReached { .. }
            );
            let _ = outcome;
            return service.error_response(protocol, &error);
        }
    };
    let (attempt, key, _skipped) = match service.open_attempt(&prepared).await {
        Ok(attempt) => attempt,
        Err(error) => return service.error_response(protocol, &error),
    };

    if !prepared.stream {
        return match crate::service::gather(&service, &prepared, attempt, &key).await {
            Ok(value) => json_response(StatusCode::OK, &value),
            Err(error) => service.error_response(protocol, &error),
        };
    }

    let (sender, receiver) = mpsc::channel::<Result<bytes::Bytes, GatewayError>>(16);
    let service_for_forward = Arc::clone(&service);
    let prepared_for_forward = prepared.clone();
    tokio::spawn(async move {
        crate::service::forward(
            service_for_forward,
            prepared_for_forward,
            attempt,
            &key,
            sender,
        )
        .await;
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Some(Ok(bytes)) => Some((Ok::<_, GatewayError>(bytes), receiver)),
            Some(Err(error)) => Some((Err(error), receiver)),
            None => None,
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    // A reverse proxy buffering the stream would turn a token stream into one
    // long pause. It is only set for the unlikely case of a proxy in between.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

async fn list_models(State(state): State<GatewayRouterState>, headers: HeaderMap) -> Response {
    if let Err(error) = perimeter(&headers) {
        return state
            .service
            .error_response(WireProtocol::ChatCompletions, &error);
    }
    let snapshot = match state.service.cached_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return state
                .service
                .error_response(WireProtocol::ChatCompletions, &error);
        }
    };
    let catalog = snapshot.model_catalog();
    let data = catalog
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "object": "model",
                "owned_by": entry.provider_name,
            })
        })
        .collect::<Vec<_>>();
    // The Config Center reads this list through the Provider model-fetch path,
    // which only understands the OpenAI `data` envelope.
    let body = serde_json::json!({"object": "list", "data": data});
    json_response(StatusCode::OK, &body)
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": {
                "message": "the gateway does not serve this path",
                "type": "not_found",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn perimeter_accepts_loopback_hosts() {
        for host in ["127.0.0.1:3425", "localhost:3425", "[::1]:3425"] {
            assert!(
                perimeter(&headers(&[("host", host)])).is_ok(),
                "{host} should be accepted"
            );
        }
    }

    #[test]
    fn perimeter_rejects_a_foreign_host_or_a_browser_origin() {
        assert!(perimeter(&headers(&[("host", "evil.example:3425")])).is_err());
        assert!(perimeter(&headers(&[])).is_err());
        assert!(
            perimeter(&headers(&[
                ("host", "127.0.0.1:3425"),
                ("origin", "https://evil.example"),
            ]))
            .is_err()
        );
        // A loopback Origin is still a browser.
        assert!(
            perimeter(&headers(&[
                ("host", "127.0.0.1:3425"),
                ("origin", "http://127.0.0.1:3425"),
            ]))
            .is_err()
        );
    }
}
