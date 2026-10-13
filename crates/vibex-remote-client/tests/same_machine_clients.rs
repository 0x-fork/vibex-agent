#![cfg(not(target_family = "wasm"))]

//! A runtime that owns a home can serve clients running on the same machine
//! without Remote Access being enabled at all.
//!
//! This is the `vibex tui` bootstrap path: another process owns the home, so the
//! client mints a device credential against the shared database and claims it
//! over the pinned-certificate Gateway narrowed to the loopback address. The
//! certificate is not received from an operator — it is derived from the
//! identity file beside that same database, which is the trust anchor reading
//! the home already implies.
//!
//! The second half of the test is the point of the whole arrangement: two
//! clients on one runtime at the same time, each with its own grant.

use std::sync::Arc;

use vibex_agent::AgentManager;
use vibex_backend::AgentBackend as _;
use vibex_core::{
    RemoteAuthProof, RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel, RequestId,
};
use vibex_db::open_database;
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteIdentityStore, RemoteTrustService,
};
use vibex_remote_client::{
    ClientDeviceIdentity, DirectWebSocketTransport, RemoteClientConfig, RemoteConnectionState,
    WebRemoteBackend, claim_pairing_code_with_identity_and_certificate,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_serves_two_same_machine_clients_at_once() {
    let root = std::env::temp_dir().join(format!("vibex-same-machine-{}", RequestId::new()));
    std::fs::create_dir_all(root.join("relay")).expect("create the home");

    let db_path = root.join("vibex.db");
    let identity_path = root.join("relay/desktop-identity.json");
    let manager = Arc::new(AgentManager::new(&db_path).expect("agent manager"));
    let gateway_config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
    let dispatcher = RemoteDispatcher::with_agent_manager(gateway_config.service.clone(), manager);
    let gateway = RemoteGateway::new(gateway_config, dispatcher, &db_path, &identity_path);

    // The listener a desktop starts for clients on this machine. It is the
    // pinned-certificate Gateway, not the plaintext Direct port.
    let listener = gateway
        .start_local_client_gateway(0)
        .await
        .expect("the loopback listener starts");
    assert!(
        listener.bound_addr.ip().is_loopback(),
        "same-machine clients must never require a network-reachable bind"
    );
    let endpoint = format!("https://127.0.0.1:{}", listener.bound_addr.port());

    // What the client can read without any network round trip: the runtime
    // identity and, from it, the certificate the listener terminates TLS with.
    let identity = RemoteIdentityStore::new(&identity_path)
        .load_or_create()
        .expect("the runtime identity");
    let pinned = vibex_remote::pinned_tls_certificate_base64(&identity)
        .expect("the pinned certificate is derived from the identity");
    assert_eq!(
        pinned, listener.tls_certificate_base64,
        "the certificate a client derives must be the one the listener serves"
    );

    let mut connection = open_database(&db_path).expect("open the shared database");
    vibex_db::apply_migrations(&mut connection).expect("migrate the shared database");

    let mut clients = Vec::new();
    let mut device_ids = Vec::new();
    for display_name in ["Vibex TUI", "Vibex TUI (second window)"] {
        // A pairing code is single use, so each client gets its own — exactly
        // what a second `vibex` invocation does.
        let response = RemoteTrustService::create_pairing_code(
            &connection,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(120_000),
            },
        )
        .expect("mint a local pairing code");
        let bundle = claim_pairing_code_with_identity_and_certificate(
            endpoint.clone(),
            response.pairing_code.clone(),
            display_name,
            false,
            Some(pinned.clone()),
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("{display_name} could not claim: {}", error.message));

        let identity = ClientDeviceIdentity::from_private_key_base64(
            bundle.credential.auth.device_id.clone(),
            &bundle.identity.private_key_base64(),
        )
        .expect("bind the claimed identity");
        device_ids.push(bundle.credential.auth.device_id.clone());
        let mut config = RemoteClientConfig::new(
            bundle.credential.server_url.clone(),
            RemoteAuthProof {
                device_id: bundle.credential.auth.device_id.clone(),
                auth_token: bundle.credential.auth.auth_token.clone(),
            },
        )
        .with_device_identity(identity);
        config.expected_server_id = Some(bundle.server_id.clone());
        config.expected_server_identity_public_key =
            bundle.credential.server_identity_public_key.clone();
        config.pinned_tls_certificate_der = Some(pinned.clone());
        config.client_id = "vibex-tui".to_string();
        config.validate().expect("the claimed route is usable");

        let backend = WebRemoteBackend::from_direct(
            DirectWebSocketTransport::new(config).expect("construct the pinned transport"),
        );
        backend.connect().await.expect("connect the pinned route");
        backend
            .transport()
            .heartbeat()
            .await
            .expect("heartbeat the pinned route");
        clients.push(backend);
    }

    assert_ne!(
        device_ids[0], device_ids[1],
        "every client holds its own revocable grant rather than sharing one"
    );

    // Both grants are live against the one authority at the same time.
    for backend in &clients {
        assert_eq!(
            backend.connection_state().state,
            RemoteConnectionState::Online,
            "a second client must not evict the first"
        );
        backend
            .list_sessions(false)
            .await
            .expect("a same-machine client can read the shared state");
    }

    for backend in clients {
        backend.disconnect().await.expect("disconnect");
    }
    gateway.stop().await.expect("stop the Gateway");
    drop(connection);
    let _ = std::fs::remove_dir_all(root);
}
