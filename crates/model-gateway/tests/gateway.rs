//! End-to-end coverage: a real loopback listener, a real upstream, and the
//! seeded local Provider Profiles.
//!
//! The mock upstream is a second axum server on loopback. Each test configures
//! one shared Provider Profile whose Key is stored through the same secret
//! store the runtime uses, points the gateway at it, and drives a full call.
//! That exercises the parts unit tests cannot: the shape of the outbound
//! request, the auth header, the streamed frames, and the ledger row.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use vibex_config_switch::ProviderConfigService;
use vibex_config_switch::secrets;
use vibex_core::{
    AgentCredential, AgentCredentialStatus, GatewayConversationPin, GatewayFailureKind,
    GatewayPolicy, GatewayRequestOutcome, GatewayRoutingStrategy, GatewayTokenWindow,
    ModelProviderAccountState, ModelProviderCatalogEntry, ModelProviderCredentialReference,
    ModelProviderEndpoint, ModelProviderEndpointKind, ModelProviderProfileId,
    ModelProviderProfileStatus, ModelProviderProxyPolicy, ProjectionSecretReference,
    ProviderSecretSetupState, RequestId,
};
use vibex_db::{
    GatewayConversationPinRepository, GatewayTokenWindowRepository,
    ModelGatewayRequestLogRepository, ModelProviderAccountStateRepository, apply_migrations,
    open_database,
};
use vibex_model_gateway::{
    GatewayLimits, ModelGatewayService, WireProtocol, build_router, classify_failure,
};

/// What the mock upstream recorded.
#[derive(Debug, Default)]
struct Recorded {
    path: String,
    authorization: Option<String>,
    api_key: Option<String>,
    body: Option<Value>,
}

type Recorder = Arc<Mutex<Recorded>>;

#[derive(Clone)]
struct MockState {
    recorder: Recorder,
    /// The status the mock answers with.
    status: u16,
    /// The SSE body the mock answers with.
    stream: String,
    /// Whether the mock answers a non-streaming request with JSON.
    json: Option<Value>,
}

async fn mock_handler(
    State(state): State<MockState>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    {
        let mut recorded = state.recorder.lock().await;
        recorded.path = uri.path().to_string();
        recorded.authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        recorded.api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        recorded.body = serde_json::from_slice(&body).ok();
    }
    if state.status != 200 {
        let mut response = Response::new(Body::from(
            json!({"error": {"message": "the upstream refused this key"}}).to_string(),
        ));
        *response.status_mut() = axum::http::StatusCode::from_u16(state.status).unwrap();
        return response;
    }
    if let Some(body) = state.json {
        let mut response = Response::new(Body::from(body.to_string()));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        return response;
    }
    let mut response = Response::new(Body::from(state.stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}

async fn mock_upstream(
    status: u16,
    stream: String,
    json: Option<Value>,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let state = MockState {
        recorder: Arc::clone(&recorder),
        status,
        stream,
        json,
    };
    let router = Router::new()
        .route("/{*path}", post(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), recorder, task)
}

fn database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vibex-gateway-it-{}.db", RequestId::new().as_str()))
}

/// Creates a shared Provider Profile with one stored Key.
fn seed_upstream(
    service: &ProviderConfigService,
    endpoint: &str,
    protocol: WireProtocol,
    models: &[&str],
    api_key: &str,
) -> ModelProviderProfileId {
    let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
    secrets::store_provider_secret(&lookup_key, api_key).unwrap();
    let credential = ModelProviderCredentialReference {
        id: RequestId::new(),
        display_name: "Primary".to_string(),
        status: AgentCredentialStatus::Ready,
        credential: AgentCredential::ApiKey {
            secret: ProjectionSecretReference {
                id: RequestId::new(),
                backend: secrets::provider_secret_write_backend(),
                setup_state: ProviderSecretSetupState::Available,
                lookup_key,
                redacted_hint: "stored".to_string(),
                revision: 1,
                legacy_secret_reference_id: None,
            },
            target_hint: None,
        },
        revision: 1,
    };
    let profile = vibex_core::ModelProviderProfileCreateRequest {
        display_name: "DeepSeek".to_string(),
        vendor_hint: Some("deepseek".to_string()),
        endpoints: vec![ModelProviderEndpoint {
            id: "endpoint-api".to_string(),
            kind: ModelProviderEndpointKind::Api,
            url: endpoint.to_string(),
            wire_protocol_id: Some(protocol.wire_protocol_id().to_string()),
        }],
        proxy_policy: ModelProviderProxyPolicy::InheritSystem,
        credentials: vec![credential],
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
    };
    service.create_model_provider_profile(profile).unwrap().id
}

fn point_gateway_at(
    path: &std::path::Path,
    upstream: ModelProviderProfileId,
    policy: GatewayPolicy,
) {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let settings = vibex_db::ModelGatewaySettingsRepository::get_or_create(&connection).unwrap();
    let mut next = settings.clone();
    next.revision = settings.revision + 1;
    next.upstreams = vec![upstream];
    next.policy = policy;
    vibex_db::ModelGatewaySettingsRepository::update(&connection, &next, settings.revision)
        .unwrap();
}

async fn gateway(
    path: &std::path::Path,
) -> (
    Arc<ModelGatewayService>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let config = ProviderConfigService::new(path);
    let service = ModelGatewayService::new(path, config, GatewayLimits::default());
    let state = service.start().await.unwrap();
    assert_eq!(state.status, vibex_core::ModelGatewayStatus::Running);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let router = build_router(Arc::clone(&service));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (service, format!("http://{address}"), task)
}

fn cleanup(path: std::path::PathBuf) {
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

fn chat_stream() -> String {
    [
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"content":"hello"}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"content":" world"}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n"
}

fn anthropic_stream() -> String {
    [
        r#"event: message_start
data: {"type":"message_start","message":{"id":"m1","model":"claude","usage":{"input_tokens":9}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ]
    .join("\n\n")
        + "\n\n"
}

#[tokio::test]
async fn passthrough_stream_reaches_the_client_and_the_ledger() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test-123",
    );
    point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "say hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("hello"), "{body}");
    assert!(body.contains("[DONE]"), "{body}");

    let recorded = recorder.lock().await;
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(
        recorded.authorization.as_deref(),
        Some("Bearer sk-test-123")
    );
    assert_eq!(recorded.body.as_ref().unwrap()["model"], "deepseek-chat");

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(
        logs[0].wire_in,
        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
    );
    assert_eq!(logs[0].model_requested, "deepseek/deepseek-chat");
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);
    assert_eq!(logs[0].input_tokens, 11);
    assert_eq!(logs[0].output_tokens, 3);
    let encoded = serde_json::to_string(&logs).unwrap().to_lowercase();
    assert!(!encoded.contains("sk-test-123"), "the ledger leaked a Key");
    assert!(!encoded.contains("say hi"), "the ledger leaked a prompt");

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_translated_call_is_re_encoded_for_the_client_protocol() {
    let path = database_path();
    // The upstream speaks Anthropic; the client speaks Chat.
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, anthropic_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::AnthropicMessages,
        &["claude-sonnet-4-5"],
        "sk-ant-test",
    );
    point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/claude-sonnet-4-5",
            "messages": [{"role": "user", "content": "say hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    // Chat SSE frames, not Anthropic ones.
    assert!(body.contains("chat.completion.chunk"), "{body}");
    assert!(body.contains("\"content\":\"hi\""), "{body}");
    assert!(!body.contains("message_start"), "{body}");
    assert!(body.contains("[DONE]"), "{body}");

    let recorded = recorder.lock().await;
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.api_key.as_deref(), Some("sk-ant-test"));
    assert_eq!(
        recorded.body.as_ref().unwrap()["model"],
        "claude-sonnet-4-5"
    );
    // Anthropic requires `max_tokens`.
    assert!(recorded.body.as_ref().unwrap()["max_tokens"].is_number());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_non_streaming_client_gets_one_body() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(
        200,
        chat_stream(),
        Some(json!({
            "id": "c1",
            "object": "chat.completion",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello world"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        })),
    )
    .await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "say hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello world");
    assert_eq!(body["usage"]["prompt_tokens"], 5);
    let recorded = recorder.lock().await;
    assert_eq!(recorded.body.as_ref().unwrap()["stream"], false);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_invalid_key_is_marked_and_the_next_key_answers() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-good",
    );
    // A second credential that does not resolve to a stored value behaves like
    // an unusable Key; the first one still answers.
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials.push(ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Broken".to_string(),
            status: AgentCredentialStatus::Missing,
            credential: AgentCredential::Local { runtime: None },
            revision: 1,
        });
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
    }
    point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-good")
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_authentication_failure_cools_the_key_until_it_is_replaced() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(401, String::new(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-revoked",
    );
    point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].cooldown_reason, Some(GatewayFailureKind::Auth));
    // An auth failure has no duration: the Key never recovers on its own.
    assert!(states[0].cooldown_until_ms.is_none());
    assert!(!states[0].is_ready_at(vibex_core::unix_timestamp_ms()));

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_cooling_key_is_skipped_and_the_second_key_answers() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-second",
    );
    // Add a higher-priority Key and mark it as cooling.
    let cooled = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-cooling").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Cooling".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: secrets::provider_secret_write_backend(),
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key,
                    redacted_hint: "stored".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,
        };
        profile.credentials.insert(0, credential.clone());
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        let mut state = ModelProviderAccountState::ready(upstream.clone(), credential.id.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(vibex_core::unix_timestamp_ms() + 60_000);
        ModelProviderAccountStateRepository::upsert(&connection, &state).unwrap();
        credential.id
    };
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-second")
    );
    let logs = service.recent_requests(10).unwrap();
    assert_ne!(logs[0].credential_id.as_ref(), Some(&cooled));

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn all_keys_cooling_is_a_429_without_an_upstream_call() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-only",
    );
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let mut state =
            ModelProviderAccountState::ready(upstream.clone(), profile.credentials[0].id.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Credit);
        state.cooldown_until_ms = Some(vibex_core::unix_timestamp_ms() + 60_000);
        ModelProviderAccountStateRepository::upsert(&connection, &state).unwrap();
    }
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_gateway_keys_cooling");
    // Nothing reached the upstream.
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_unknown_model_lists_the_closest_configured_ones() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat", "deepseek-reasoner"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepsek-chat",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("deepseek-chat")
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_vendor_side_file_reference_fails_loudly_before_any_call() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    // The upstream speaks Chat only, so a `file_id` image cannot cross over.
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_file", "file_id": "file-abc"}
                ]}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unsupported_image_reference");
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_conversation_stays_on_its_pinned_key() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-first",
    );
    point_gateway_at(
        &path,
        upstream.clone(),
        GatewayPolicy {
            routing: GatewayRoutingStrategy::RoundRobin,
            ..GatewayPolicy::default()
        },
    );
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let client = reqwest::Client::new();
    for turn in 0..3 {
        let response = client
            .post(format!("{gateway_url}/v1/chat/completions"))
            .header(header::HOST, "127.0.0.1")
            .header("x-session-id", "conversation-1")
            .json(&json!({
                "model": "deepseek/deepseek-chat",
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hi"}
                ],
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "turn {turn}");
        let _ = response.text().await.unwrap();
    }
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-first")
    );

    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let pins = connection
        .query_row(
            "SELECT COUNT(*) FROM model_gateway_conversation_pin",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(pins, 1);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_browser_origin_is_refused() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .header(header::ORIGIN, "http://127.0.0.1:3425")
        .json(&json!({"model": "deepseek/deepseek-chat", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_ne!(response.status(), 200);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn the_token_ceiling_stops_requests_and_the_window_rolls_over() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    point_gateway_at(
        &path,
        upstream.clone(),
        GatewayPolicy {
            token_limit: Some(vibex_core::GatewayTokenLimit {
                window: GatewayTokenWindow::Day,
                tokens: 10,
            }),
            ..GatewayPolicy::default()
        },
    );
    // The stream reports 11 input + 3 output tokens, so one call crosses a
    // ceiling of 10.
    GatewayTokenWindowRepository::record(
        &{
            let mut connection = open_database(&path).unwrap();
            apply_migrations(&mut connection).unwrap();
            connection
        },
        &upstream,
        GatewayTokenWindow::Day,
        10,
        vibex_core::unix_timestamp_ms(),
    )
    .unwrap();
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_gateway_token_ceiling_reached");
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[test]
fn failure_classification_is_shared_with_the_service() {
    assert_eq!(classify_failure(401, ""), GatewayFailureKind::Auth);
    assert_eq!(
        classify_failure(429, "rate limit exceeded"),
        GatewayFailureKind::Rate
    );
}

#[test]
fn pins_round_trip_through_the_database() {
    let path = database_path();
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let upstream = vibex_db::ModelProviderProfileRepository::list(&connection)
        .unwrap()
        .first()
        .map(|profile| profile.id.clone())
        .expect("seeded profiles");
    let now = vibex_core::unix_timestamp_ms();
    GatewayConversationPinRepository::upsert(
        &connection,
        &GatewayConversationPin {
            conversation_key: "k".to_string(),
            model_provider_profile_id: upstream,
            credential_id: RequestId::new(),
            chosen_at_ms: now,
            expires_at_ms: now + GatewayConversationPin::TTL_MS,
        },
    )
    .unwrap();
    assert!(
        GatewayConversationPinRepository::get(&connection, "k")
            .unwrap()
            .is_some()
    );
    cleanup(path);
}

#[test]
fn ledger_retention_is_seven_days() {
    let path = database_path();
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let upstream = vibex_db::ModelProviderProfileRepository::list(&connection)
        .unwrap()
        .first()
        .map(|profile| profile.id.clone())
        .expect("seeded profiles");
    let now = vibex_core::unix_timestamp_ms();
    for age in [0, vibex_core::ModelGatewayRequestLog::RETENTION_MS + 1] {
        ModelGatewayRequestLogRepository::insert(
            &connection,
            &vibex_core::ModelGatewayRequestLog {
                request_id: RequestId::new(),
                agent_id: Some("codex".to_string()),
                model_provider_profile_id: upstream.clone(),
                credential_id: None,
                model_requested: "m".to_string(),
                model_served: None,
                wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                status: Some(200),
                input_tokens: 1,
                output_tokens: 1,
                cached_read_tokens: 0,
                cached_write_tokens: 0,
                reasoning_tokens: 0,
                latency_ms: Some(1),
                outcome: GatewayRequestOutcome::Ok,
                created_at_ms: now - age,
            },
        )
        .unwrap();
    }
    assert_eq!(
        ModelGatewayRequestLogRepository::prune(&connection, now).unwrap(),
        1
    );
    cleanup(path);
}
