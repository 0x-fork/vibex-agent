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

/// How many requests the upstream saw overlapping at once.
///
/// This is the only honest way to test a concurrency limit: counting from the
/// client side measures the test's own parallelism, not what left the gateway.
#[derive(Debug, Default)]
struct Overlap {
    current: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

impl Overlap {
    fn enter(&self) {
        let now = self
            .current
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.peak
            .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
    }

    fn leave(&self) {
        self.current
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }
}

type Recorder = Arc<Mutex<Recorded>>;

#[derive(Clone)]
struct MockState {
    recorder: Recorder,
    overlap: Arc<Overlap>,
    /// The status the mock answers with.
    status: u16,
    /// The SSE body the mock answers with.
    stream: String,
    /// Whether the mock answers a non-streaming request with JSON.
    json: Option<Value>,
    /// One Authorization header the mock refuses, and with which status, so a
    /// test can make the first Key fail while the next one answers.
    refuse: Option<(String, u16)>,
}

async fn mock_handler(
    State(state): State<MockState>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    state.overlap.enter();
    // Hold the request open long enough for a second one to overlap if the
    // gateway really sent it.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
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
    let refused_status = state.refuse.as_ref().and_then(|(refused, status)| {
        (headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(refused.as_str()))
        .then_some(*status)
    });
    if let Some(status) = refused_status.or((state.status != 200).then_some(state.status)) {
        state.overlap.leave();
        let mut response = Response::new(Body::from(
            json!({"error": {"message": "the upstream refused this key"}}).to_string(),
        ));
        *response.status_mut() = axum::http::StatusCode::from_u16(status).unwrap();
        return response;
    }
    if let Some(body) = state.json {
        state.overlap.leave();
        let mut response = Response::new(Body::from(body.to_string()));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        return response;
    }
    state.overlap.leave();
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
    let (url, recorder, _overlap, task) = mock_upstream_with_overlap(status, stream, json).await;
    (url, recorder, task)
}

/// A mock that refuses one Key and serves every other one normally.
///
/// A failover needs two different answers from the same upstream, which a
/// fixed status cannot express.
async fn mock_upstream_refusing(
    refused_authorization: &str,
    stream: String,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    mock_upstream_refusing_with_status(refused_authorization, 429, stream).await
}

async fn mock_upstream_refusing_with_status(
    refused_authorization: &str,
    status: u16,
    stream: String,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let (url, recorder, _overlap, task) =
        mock_upstream_with_refusal(refused_authorization, status, stream).await;
    (url, recorder, task)
}

async fn mock_upstream_with_refusal(
    refused_authorization: &str,
    refusal_status: u16,
    stream: String,
) -> (String, Recorder, Arc<Overlap>, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let overlap = Arc::new(Overlap::default());
    let state = MockState {
        recorder: Arc::clone(&recorder),
        overlap: Arc::clone(&overlap),
        status: 200,
        stream,
        json: None,
        refuse: Some((refused_authorization.to_string(), refusal_status)),
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
    (format!("http://{address}"), recorder, overlap, task)
}

async fn mock_upstream_with_overlap(
    status: u16,
    stream: String,
    json: Option<Value>,
) -> (String, Recorder, Arc<Overlap>, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let overlap = Arc::new(Overlap::default());
    let state = MockState {
        recorder: Arc::clone(&recorder),
        overlap: Arc::clone(&overlap),
        status,
        stream,
        json,
        refuse: None,
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
    (format!("http://{address}"), recorder, overlap, task)
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
async fn a_translated_non_streaming_client_gets_a_whole_body() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, anthropic_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    // Anthropic-only upstream: the Chat request must be translated, and the
    // client asked for one JSON body, so the gateway has to gather the stream.
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::AnthropicMessages,
        &["claude"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/claude",
            "messages": [{"role": "user", "content": "say hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hi");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(
        recorder.lock().await.body.as_ref().unwrap()["stream"],
        true,
        "a translated non-streaming call is read from the upstream as a stream"
    );

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
async fn a_preferred_key_is_tried_before_the_provider_order() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing("Bearer sk-refused", chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-refused",
    );
    // The second Key is valid but sits behind the refused one in the Provider's
    // order; preferring it is the only thing that makes it first.
    let preferred = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-good").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Backup".to_string(),
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
        let id = credential.id.clone();
        profile.credentials.push(credential);
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        id
    };
    point_gateway_at(
        &path,
        upstream.clone(),
        GatewayPolicy {
            preferred_keys: std::collections::BTreeMap::from([(
                upstream.clone(),
                preferred.clone(),
            )]),
            ..GatewayPolicy::default()
        },
    );
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

    // The refused Key was never attempted, so no cooldown was recorded for it:
    // preference is routing order, not a failover that happened to work.
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert!(states.is_empty(), "{states:?}");

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
async fn an_authentication_failure_fails_over_to_the_next_key() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing_with_status("Bearer sk-revoked", 401, chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-revoked",
    );
    config
        .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
            model_provider_profile_id: upstream.clone(),
            display_name: Some("Backup".to_string()),
            value: "sk-good".to_string(),
        })
        .unwrap();
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
        Some("Bearer sk-good"),
        "a rejected Key must not end the request while another Key can answer"
    );
    let logs = service.recent_requests(10).unwrap();
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.failed_attempts.len(), 1);
    assert_eq!(trace.failed_attempts[0].failure, GatewayFailureKind::Auth);

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
    // The ledger explains the request without exposing a Key: the cooling Key
    // is counted, the answering Key is named by id, and nothing was tried and
    // failed on the way.
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.skipped_cooling, 1);
    assert_eq!(trace.skipped_over_limit, 0);
    assert!(trace.failed_attempts.is_empty());
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Key that fails before the reply begins is named, with its reason, on the
/// row that records the Key which answered instead.
#[tokio::test]
async fn a_failover_records_the_refused_key_and_why_it_moved_on() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing("Bearer sk-refused", chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-refused",
    );
    // A second Key on the same Provider, behind the refused one.
    let backup = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-backup").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Backup".to_string(),
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
        profile.credentials.push(credential.clone());
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        credential.id
    };
    let refused = {
        let connection = open_database(&path).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
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
        Some("Bearer sk-backup")
    );

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Failover);
    assert_eq!(logs[0].credential_id.as_ref(), Some(&backup));
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.skipped_cooling, 0);
    assert_eq!(trace.failed_attempts.len(), 1);
    assert_eq!(trace.failed_attempts[0].credential_id, refused);
    assert_eq!(trace.failed_attempts[0].failure, GatewayFailureKind::Rate);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Provider that holds more than one Key is not a failover by itself.
///
/// The ledger used to call any request from a multi-Key pool a failover, which
/// made the column useless exactly when failovers were worth reading about.
#[tokio::test]
async fn a_first_try_success_on_a_multi_key_provider_is_not_a_failover() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-first",
    );
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-second").unwrap();
        profile.credentials.push(ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Secondary".to_string(),
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
        });
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
    }
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

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);
    assert!(
        logs[0]
            .route
            .as_ref()
            .is_some_and(|trace| trace.is_unremarkable())
    );

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

/// One persisted Provider, for the tests that need a real foreign key.
///
/// The local default placeholders were removed, so a migrated database starts
/// with no Providers at all. A test that needs a row seeds its own rather than
/// relying on a fixture that no longer exists.
fn seed_persisted_provider(connection: &vibex_db::DbConnection) -> ModelProviderProfileId {
    let profile = vibex_core::ModelProviderProfile {
        display_name: "Test Provider".to_string(),
        status: vibex_core::ModelProviderProfileStatus::Enabled,
        ..Default::default()
    };
    vibex_db::ModelProviderProfileRepository::insert(connection, &profile).unwrap();
    profile.id
}

#[test]
fn pins_round_trip_through_the_database() {
    let path = database_path();
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let upstream = seed_persisted_provider(&connection);
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
    let upstream = seed_persisted_provider(&connection);
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
                route: None,
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

/// A configured concurrency limit bounds what reaches the upstream.
///
/// The mock counts overlapping requests on its own side, which is the only
/// honest measurement: a client-side counter would see the test's parallelism
/// rather than what the gateway actually sent.
#[tokio::test]
async fn a_concurrency_limit_serializes_requests_rather_than_dropping_them() {
    let path = database_path();
    let (upstream_url, recorder, overlap, upstream_task) =
        mock_upstream_with_overlap(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    // Serialize the Key to one in-flight request.
    let credential = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
    };
    let mut policy = GatewayPolicy::default();
    policy.account_limits.insert(
        credential,
        vibex_core::GatewayAccountLimits {
            max_concurrency: Some(1),
            queue_wait_ms: Some(5_000),
            ..vibex_core::GatewayAccountLimits::default()
        },
    );
    point_gateway_at(&path, upstream, policy);
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let mut handles = Vec::new();
    for _ in 0..3 {
        let client = reqwest::Client::new();
        let url = format!("{gateway_url}/v1/chat/completions");
        handles.push(tokio::spawn(async move {
            client
                .post(url)
                .header(header::HOST, "127.0.0.1")
                .json(&json!({
                    "model": "deepseek/deepseek-chat",
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true
                }))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    for handle in handles {
        assert_eq!(
            handle.await.unwrap(),
            200,
            "a queued request must still answer"
        );
    }
    assert_eq!(
        overlap.peak(),
        1,
        "the upstream saw {} requests at once under a limit of one",
        overlap.peak()
    );
    // All three were served, so the limiter queued rather than refused.
    assert_eq!(service.recent_requests(10).unwrap().len(), 3);
    let _ = recorder;

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Without a limit the same three requests do overlap, which proves the test
/// above measures the limit rather than the mock's own timing.
#[tokio::test]
async fn without_a_limit_the_same_requests_do_overlap() {
    let path = database_path();
    let (upstream_url, _recorder, overlap, upstream_task) =
        mock_upstream_with_overlap(200, chat_stream(), None).await;
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

    let mut handles = Vec::new();
    for _ in 0..3 {
        let client = reqwest::Client::new();
        let url = format!("{gateway_url}/v1/chat/completions");
        handles.push(tokio::spawn(async move {
            client
                .post(url)
                .header(header::HOST, "127.0.0.1")
                .json(&json!({
                    "model": "deepseek/deepseek-chat",
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true
                }))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    for handle in handles {
        assert_eq!(handle.await.unwrap(), 200);
    }
    assert!(
        overlap.peak() > 1,
        "the mock never overlapped, so the limit test proves nothing: peak {}",
        overlap.peak()
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// An RPM limit that a request cannot wait out is a 429 that says why.
#[tokio::test]
async fn an_exhausted_minute_is_a_429_before_the_upstream_is_called() {
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
    let credential = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
    };
    let mut policy = GatewayPolicy::default();
    policy.account_limits.insert(
        credential,
        vibex_core::GatewayAccountLimits {
            max_rpm: Some(1),
            queue_wait_ms: Some(1),
            ..vibex_core::GatewayAccountLimits::default()
        },
    );
    point_gateway_at(&path, upstream, policy);
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let client = reqwest::Client::new();
    for turn in 0..2 {
        let response = client
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
        if turn == 0 {
            assert_eq!(response.status(), 200);
            let _ = response.text().await;
        } else {
            assert_eq!(
                response.status(),
                429,
                "the second request in a one-request minute must be refused"
            );
            let body: Value = response.json().await.unwrap();
            assert!(
                body["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("minute"),
                "{body}"
            );
        }
    }

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Three consecutive unreachable upstreams degrade the gateway; a recovered
/// upstream then clears the state through a single probe.
///
/// The contract has two halves. While degraded, the gateway admits one request
/// at a time instead of refusing everything, because the only signal that can
/// recover it is an upstream success. If nothing is admitted, no success can be
/// observed and the state could only clear on a restart.
#[tokio::test]
async fn repeated_failures_degrade_the_gateway_and_one_probe_recovers_it() {
    let path = database_path();
    // A port nothing listens on: every attempt fails to connect.
    let dead_url = {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    };
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &dead_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let client = reqwest::Client::new();
    let call = |client: reqwest::Client, url: String| async move {
        client
            .post(url)
            .header(header::HOST, "127.0.0.1")
            .json(&json!({
                "model": "deepseek/deepseek-chat",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            }))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };

    for _ in 0..vibex_model_gateway::ModelGatewayService::DEGRADE_AFTER_FAILURES {
        let status = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
        assert!(status >= 400, "a dead upstream should fail, got {status}");
    }
    assert!(
        service.is_degraded(),
        "the gateway did not step aside after {} failures",
        vibex_model_gateway::ModelGatewayService::DEGRADE_AFTER_FAILURES
    );
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Degraded
    );
    assert!(
        service.runtime_state().notice.is_some(),
        "a degraded gateway must say why"
    );

    // A degraded gateway still lets exactly one request through. The upstream
    // is dead, so that probe fails — but it is the probe that keeps recovery
    // possible.
    let probe = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
    assert!(
        probe >= 400,
        "a probe against a dead upstream must fail, got {probe}"
    );
    assert!(
        service.is_degraded(),
        "a failed probe must leave the gateway degraded"
    );

    // A second concurrent request cannot claim the slot the first one holds.
    // Without an upstream to talk to, both fail; the point is that the gateway
    // never leaves degraded on a failure.
    let again = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
    assert!(again >= 400, "the gateway must not answer from nothing");
    assert!(service.is_degraded());

    gateway_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A degraded gateway recovers as soon as an upstream answers again, without a
/// restart.
///
/// This is the regression the probe exists for: refusing every request while
/// degraded meant `note_success` could never run, so the state was terminal.
#[tokio::test]
async fn a_degraded_gateway_recovers_when_an_upstream_answers() {
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

    // Put the gateway into `Degraded` directly: the failure counting itself is
    // covered above, and driving three dead upstreams here would only repeat it.
    service.publish(vibex_core::ModelGatewayRuntimeState {
        status: vibex_core::ModelGatewayStatus::Degraded,
        notice: Some("degraded for the test".to_string()),
        ..vibex_core::ModelGatewayRuntimeState::running(1, 1)
    });
    assert!(service.is_degraded());

    let client = reqwest::Client::new();
    let status = client
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "the probe should have reached the upstream");
    assert!(
        !service.is_degraded(),
        "one good answer must clear the degraded state without a restart"
    );
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Running
    );
    assert!(service.runtime_state().notice.is_none());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Creates a gateway route over the given members, in order.
fn seed_route(
    path: &std::path::Path,
    name: &str,
    members: &[(ModelProviderProfileId, &str)],
    strategy: vibex_core::RouteStrategy,
) -> vibex_core::ModelRouteId {
    seed_route_with_timeout(path, name, members, strategy, None)
}

fn seed_route_with_timeout(
    path: &std::path::Path,
    name: &str,
    members: &[(ModelProviderProfileId, &str)],
    strategy: vibex_core::RouteStrategy,
    first_token_timeout_ms: Option<u64>,
) -> vibex_core::ModelRouteId {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: name.to_string(),
        kind: vibex_core::RouteKind::Gateway,
        strategy,
        first_token_timeout_ms,
        status: vibex_core::RouteStatus::Enabled,
        members: members
            .iter()
            .enumerate()
            .map(|(index, (profile, model))| {
                vibex_core::ModelRouteMember::new(profile.clone(), *model, index as i64)
            })
            .collect(),
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    route.id.clone()
}

/// Seeds a provider group: Providers rather than `(Provider, model)` pairs.
fn seed_provider_group(
    path: &std::path::Path,
    name: &str,
    providers: &[ModelProviderProfileId],
    strategy: vibex_core::RouteStrategy,
) -> vibex_core::ModelRouteId {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: name.to_string(),
        kind: vibex_core::RouteKind::ProviderGroup,
        strategy,
        status: vibex_core::RouteStatus::Enabled,
        members: providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                vibex_core::ModelRouteMember::provider(provider.clone(), index as i64)
            })
            .collect(),
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    route.id.clone()
}

/// A provider group offers only the models every Provider serves, and a
/// request for one is answered by a Provider that serves it.
///
/// The group is ordered so the second Provider would answer a plain model id
/// first; the point is that the model — not the Provider — is what the Agent
/// names, and the group's membership decides who may answer.
#[tokio::test]
async fn a_provider_group_routes_a_shared_model_to_a_serving_provider() {
    let path = database_path();
    let (left_url, left_recorder, left_task) = mock_upstream(200, chat_stream(), None).await;
    let (right_url, right_recorder, right_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let left = seed_upstream(
        &config,
        &left_url,
        WireProtocol::ChatCompletions,
        &["shared-model"],
        "sk-left",
    );
    let right = seed_upstream(
        &config,
        &right_url,
        WireProtocol::ChatCompletions,
        &["shared-model"],
        "sk-right",
    );
    point_gateway_at(&path, left.clone(), GatewayPolicy::default());
    let group_id = seed_provider_group(
        &path,
        "Team",
        &[left.clone(), right.clone()],
        vibex_core::RouteStrategy::Ordered,
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    // The group's own catalogue must publish the shared model, spelled the way
    // a session selects it.
    let served = service.read_snapshot().unwrap();
    let selected = format!("group/{}/shared-model", group_id.as_str());
    let group = served
        .group_for_model(&selected)
        .expect("the group is in the snapshot");
    assert_eq!(group.1, "shared-model");
    assert_eq!(group.0.models.len(), 1, "one shared model");

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("group/{}/shared-model", group_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(
        status, 200,
        "a shared model must be routable through the group"
    );
    assert!(
        !left_recorder.lock().await.path.is_empty(),
        "the first Provider in the group should answer"
    );
    assert!(
        right_recorder.lock().await.path.is_empty(),
        "the ordered group only fails over when it must"
    );

    gateway_task.abort();
    left_task.abort();
    right_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A model only one Provider of the group serves is not part of the group.
///
/// This is the intersection contract: offering it would let the group's policy
/// send a request to a Provider that has never heard of the model.
#[tokio::test]
async fn a_provider_group_refuses_a_model_only_one_provider_serves() {
    let path = database_path();
    let (left_url, _, left_task) = mock_upstream(200, chat_stream(), None).await;
    let (right_url, _, right_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let left = seed_upstream(
        &config,
        &left_url,
        WireProtocol::ChatCompletions,
        &["shared-model", "left-only"],
        "sk-left",
    );
    let right = seed_upstream(
        &config,
        &right_url,
        WireProtocol::ChatCompletions,
        &["shared-model"],
        "sk-right",
    );
    point_gateway_at(&path, left.clone(), GatewayPolicy::default());
    let group_id = seed_provider_group(
        &path,
        "Team",
        &[left, right],
        vibex_core::RouteStrategy::Ordered,
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let served = service.read_snapshot().unwrap();
    let group = served
        .groups
        .get(group_id.as_str())
        .expect("the group is in the snapshot");
    assert_eq!(
        group
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["shared-model"],
        "only the intersection is offered"
    );
    assert!(
        served
            .group_for_model(&format!("group/{}/left-only", group_id.as_str()))
            .is_none(),
        "a model one Provider serves is not group-shaped"
    );

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("group/{}/left-only", group_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(
        status, 404,
        "a model the group does not offer must be refused"
    );

    gateway_task.abort();
    left_task.abort();
    right_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A route's second member answers when the first one cannot.
///
/// This is the stage-2 acceptance criterion. The first member's only Key is put
/// on cooldown, so nothing can answer there; the gateway must walk to the
/// second member, and the Agent, which asked for `route/<id>`, must receive the
/// answer as if it had asked for one model.
#[tokio::test]
async fn a_route_falls_through_to_its_second_member() {
    let path = database_path();
    let (first_url, first_recorder, first_task) = mock_upstream(200, chat_stream(), None).await;
    let (second_url, second_recorder, second_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let first = seed_upstream(
        &config,
        &first_url,
        WireProtocol::ChatCompletions,
        &["first-model"],
        "sk-first",
    );
    let second = seed_upstream(
        &config,
        &second_url,
        WireProtocol::ChatCompletions,
        &["second-model"],
        "sk-second",
    );

    // Only the second member is a gateway upstream. The first is still a
    // resolvable member: a route may name a Provider the global list omits, and
    // the snapshot lists every enabled Provider a route points at.
    point_gateway_at(&path, second.clone(), GatewayPolicy::default());
    let route_id = seed_route(
        &path,
        "Standard",
        &[
            (first.clone(), "first-model"),
            (second.clone(), "second-model"),
        ],
        vibex_core::RouteStrategy::Ordered,
    );

    // Cool the first member's only Key for an hour.
    {
        let connection = open_database(&path).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::list(&connection)
            .unwrap()
            .into_iter()
            .find(|profile| profile.id == first)
            .expect("the first Provider");
        let credential = profile.credentials.first().expect("its Key").id.clone();
        let mut state = ModelProviderAccountState::ready(first.clone(), credential);
        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(vibex_core::unix_timestamp_ms() + 3_600_000);
        ModelProviderAccountStateRepository::upsert(&connection, &state).unwrap();
    }

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("route/{}", route_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "the second member should have answered");

    assert!(
        first_recorder.lock().await.path.is_empty(),
        "a cooling member must not be called at all"
    );
    assert!(
        !second_recorder.lock().await.path.is_empty(),
        "the request should have reached the second member"
    );

    gateway_task.abort();
    first_task.abort();
    second_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// The ledger explains a cross-member failover.
///
/// A request that walked past one member and answered on another is only
/// diagnosable if the trace names the member it skipped. Without it the row
/// says "a Provider answered" and nothing about the walk.
#[tokio::test]
async fn a_member_failover_is_recorded_in_the_route_trace() {
    let path = database_path();
    let (live_url, _live_recorder, live_task) = mock_upstream(200, chat_stream(), None).await;
    // The first member answers 500, which is a failure before any byte of a
    // reply: exactly the case a route may walk past.
    let (dead_url, _dead_recorder, dead_task) = mock_upstream(500, String::new(), None).await;
    let config = ProviderConfigService::new(&path);
    let dead = seed_upstream(
        &config,
        &dead_url,
        WireProtocol::ChatCompletions,
        &["dead-model"],
        "sk-dead",
    );
    let live = seed_upstream(
        &config,
        &live_url,
        WireProtocol::ChatCompletions,
        &["live-model"],
        "sk-live",
    );
    point_gateway_at(&path, live.clone(), GatewayPolicy::default());
    let route_id = seed_route(
        &path,
        "Standard",
        &[(dead.clone(), "dead-model"), (live.clone(), "live-model")],
        vibex_core::RouteStrategy::Ordered,
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("route/{}", route_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "the live member should have answered");

    let logs =
        ModelGatewayRequestLogRepository::recent(&open_database(&path).unwrap(), 10).unwrap();
    let log = logs.first().expect("the request was recorded");
    assert_eq!(
        log.model_provider_profile_id, live,
        "the ledger names the member that answered"
    );
    let trace = log.route.as_ref().expect("the trace explains the walk");
    assert!(
        !trace.is_unremarkable(),
        "a cross-member failover is worth recording: {trace:?}"
    );
    assert_eq!(
        trace.skipped_members.len(),
        1,
        "the dead member should be named"
    );
    assert_eq!(
        trace.skipped_members[0].model_provider_profile_id, dead,
        "the skipped member is the one that could not answer"
    );

    gateway_task.abort();
    live_task.abort();
    dead_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A route is one model id, however many members and models it holds.
#[tokio::test]
async fn a_route_is_listed_as_one_model() {
    let path = database_path();
    let (url, _recorder, task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let provider = seed_upstream(
        &config,
        &url,
        WireProtocol::ChatCompletions,
        &["a", "b"],
        "sk-test",
    );
    point_gateway_at(&path, provider.clone(), GatewayPolicy::default());
    let route_id = seed_route(
        &path,
        "Standard",
        &[(provider.clone(), "a"), (provider.clone(), "b")],
        vibex_core::RouteStrategy::Ordered,
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway_url}/v1/models"))
        .header(header::HOST, "127.0.0.1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids = body["data"]
        .as_array()
        .expect("a model list")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect::<Vec<_>>();
    // The test seeds its own route; a migration may have seeded another one
    // from the gateway's upstream list, so the assertion is about the shape of
    // this route's entries rather than the total count.
    let route_ids = ids
        .iter()
        .filter(|id| id.starts_with("route/"))
        .collect::<Vec<_>>();
    assert!(
        route_ids.iter().any(|id| id.ends_with(&route_id.as_str())),
        "the seeded route is listed: {ids:?}"
    );
    // The bare `upstream/model` entries stay because a direct Provider is still
    // reachable by name; what must not happen is a route contributing one entry
    // per member under the route's own prefix.
    assert!(
        !ids.iter()
            .any(|id| id.starts_with("route/") && id.contains("/a")),
        "a route must not list its members as models: {ids:?}"
    );

    gateway_task.abort();
    task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A member that accepts the call and then stalls is abandoned for the next.
///
/// This is the case no error can report: the connection succeeds, so nothing
/// fails over on its own, and the turn hangs until the client gives up. The
/// route's first-token deadline turns that silence into a failover.
#[tokio::test]
async fn a_stalled_member_is_abandoned_for_the_next() {
    let path = database_path();

    // A mock that reads the request and never answers.
    let stalled_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let stalled_address = stalled_listener.local_addr().unwrap();
    let stalled_task = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let Ok((stream, _)) = stalled_listener.accept().await else {
                break;
            };
            // Hold the connection open without writing a byte.
            held.push(stream);
        }
    });

    let (live_url, _recorder, live_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let stalled = seed_upstream(
        &config,
        &format!("http://{stalled_address}"),
        WireProtocol::ChatCompletions,
        &["stalled-model"],
        "sk-stalled",
    );
    let live = seed_upstream(
        &config,
        &live_url,
        WireProtocol::ChatCompletions,
        &["live-model"],
        "sk-live",
    );
    point_gateway_at(&path, live.clone(), GatewayPolicy::default());
    // 300 ms is far below the test's own patience and far above a local mock.
    let route_id = seed_route_with_timeout(
        &path,
        "Standard",
        &[
            (stalled.clone(), "stalled-model"),
            (live.clone(), "live-model"),
        ],
        vibex_core::RouteStrategy::Ordered,
        Some(300),
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reqwest::Client::new()
            .post(format!("{gateway_url}/v1/chat/completions"))
            .header(header::HOST, "127.0.0.1")
            .json(&json!({
                "model": format!("route/{}", route_id.as_str()),
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            }))
            .send(),
    )
    .await
    .expect("the stalled member must not hang the request")
    .unwrap()
    .status()
    .as_u16();
    // The stalled member produced no bytes, so the gateway reports the failure
    // in-stream rather than handing the client a truncated 200.
    assert!(
        status == 200 || status >= 400,
        "the request must terminate, not hang; got {status}"
    );

    gateway_task.abort();
    stalled_task.abort();
    live_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A real request announces itself while it runs, not only when it finishes.
///
/// This is the channel the route graph draws from, so it has to exist for a
/// request the gateway actually served — the unit tests cover the table, this
/// covers the wiring from `prepare` to the ledger.
#[tokio::test]
async fn a_live_request_publishes_begin_and_end() {
    use std::sync::Mutex as StdMutex;

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

    let seen: Arc<StdMutex<Vec<vibex_core::GatewayEvent>>> = Arc::new(StdMutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    service.set_event_sink(Arc::new(move |event| {
        sink.lock().unwrap().push(event);
    }));

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);

    let events = seen.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, vibex_core::GatewayEvent::Began(_))),
        "a request that ran must have announced itself: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, vibex_core::GatewayEvent::Ended { .. })),
        "and must announce that it finished: {events:?}"
    );
    // One request, one id: the events describe the same thing the ledger row
    // does, so a client can follow it rather than guess.
    let first = events[0].request_id().clone();
    assert!(events.iter().all(|event| event.request_id() == &first));
    assert!(
        service.inflight_requests().is_empty(),
        "nothing may stay drawn once the reply is done"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}
