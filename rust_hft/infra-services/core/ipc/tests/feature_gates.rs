//! Feature gate integration tests
//!
//! These tests verify that the crate can be used in different feature configurations.

use hft_ipc::{Command, IPCMessage, IPCPayload, Response};

#[test]
fn message_types_available_without_features() {
    // Message type definitions should always be available
    let command = Command::GetStatus;
    let payload = IPCPayload::Command(command);
    let message = IPCMessage::new(payload);

    // Should be able to create and use message structures
    assert!(message.timestamp > 0);
}

#[test]
fn message_serialization_works() {
    use serde_json;

    // Message types should be serializable even without IPC feature
    let command = Command::GetStatus;
    let json = serde_json::to_string(&command).unwrap();
    assert!(json.contains("GetStatus"));
}

#[test]
fn operator_account_cancel_and_replace_commands_roundtrip() {
    let commands = [
        Command::InspectExecutionAccounts,
        Command::CancelOrdersFiltered {
            symbol: Some(hft_core::Symbol::new("123")),
            venue: Some("POLYMARKET".to_string()),
        },
        Command::CancelOrderById {
            order_id: "venue-order-1".to_string(),
        },
        Command::ReplaceOrder {
            order_id: "venue-order-1".to_string(),
            symbol: hft_core::Symbol::new("123"),
            new_quantity: Some(rust_decimal::Decimal::ONE),
            new_price: Some(rust_decimal::Decimal::new(51, 2)),
        },
    ];

    for command in commands {
        let encoded = serde_json::to_string(&command).expect("serialize operator command");
        let decoded: Command =
            serde_json::from_str(&encoded).expect("deserialize operator command");
        assert_eq!(
            std::mem::discriminant(&decoded),
            std::mem::discriminant(&command)
        );
    }
}

#[cfg(feature = "ipc")]
#[test]
fn ipc_functionality_available_with_feature() {
    // IPC server and client should only be available with 'ipc' feature
    // This test simply verifies that the modules are accessible with the feature enabled

    // We can't easily instantiate these without a full handler implementation,
    // but we can verify the types exist by referencing them in a type annotation
    use hft_ipc::{IPCClient, DEFAULT_SOCKET_PATH};

    // Verify IPCClient type exists and we can reference its path
    let _path: &str = DEFAULT_SOCKET_PATH;

    // This ensures the IPC module and client type are accessible
    let _phantom: fn() -> IPCClient =
        || unreachable!("This is never called, just for type checking");
}

#[cfg(feature = "ipc")]
#[test]
fn request_id_is_uuid_with_feature() {
    use hft_ipc::RequestId;

    // With IPC feature, RequestId should be UUID
    let id: RequestId = uuid::Uuid::new_v4();
    assert!(!id.to_string().is_empty());
}

#[cfg(not(feature = "ipc"))]
#[test]
fn request_id_is_string_without_feature() {
    use hft_ipc::RequestId;

    // Without IPC feature, RequestId should be String
    let id: RequestId = String::from("test-id");
    assert_eq!(id, "test-id");
}

#[test]
fn all_command_variants_available() {
    // Verify all command variants can be constructed
    let commands = [
        Command::Start,
        Command::Stop,
        Command::EmergencyStop,
        Command::GetStatus,
        Command::GetAccount,
        Command::GetPositions,
        Command::GetOpenOrders,
        Command::CancelAllOrders,
    ];

    assert_eq!(commands.len(), 8);
}

#[test]
fn response_types_available() {
    // Verify response types can be constructed
    let responses = [
        Response::Ok,
        Response::Error {
            message: "test error".to_string(),
            code: Some(500),
        },
    ];

    assert_eq!(responses.len(), 2);
}

#[cfg(feature = "ipc")]
#[test]
fn serialization_roundtrip_with_messagepack() {
    use rmp_serde::{decode, encode};

    // With IPC feature, MessagePack serialization should work
    let command = Command::GetStatus;
    let payload = IPCPayload::Command(command.clone());
    let message = IPCMessage::new(payload);

    // Serialize
    let bytes = encode::to_vec(&message).expect("Failed to serialize");
    assert!(!bytes.is_empty());

    // Deserialize
    let decoded: IPCMessage = decode::from_slice(&bytes).expect("Failed to deserialize");

    match decoded.payload {
        IPCPayload::Command(Command::GetStatus) => {}
        _ => panic!("Wrong payload type after roundtrip"),
    }
}

#[cfg(feature = "ipc")]
#[tokio::test]
async fn ipc_server_requires_the_request_token_and_never_echoes_it() {
    use hft_ipc::{handlers::MockCommandHandler, IPCClient, IPCServer};

    let directory = tempfile::tempdir().expect("temporary IPC directory");
    let socket_path = directory.path().join("authenticated.sock");
    let server = IPCServer::new_with_auth(
        &socket_path,
        MockCommandHandler,
        Some("expected-token".to_string()),
    );
    let server_task = tokio::spawn(async move { server.start().await });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        socket_path.exists(),
        "authenticated IPC socket did not bind"
    );

    let denied = IPCClient::new(&socket_path)
        .with_auth_token("wrong-token")
        .get_status()
        .await
        .expect("authentication rejection response");
    assert!(matches!(
        denied,
        Response::Error {
            code: Some(401),
            ..
        }
    ));
    assert!(IPCClient::new(&socket_path)
        .with_auth_token("wrong-token")
        .subscribe_status()
        .await
        .is_err());
    let accepted = IPCClient::new(&socket_path)
        .with_auth_token("expected-token")
        .get_status()
        .await
        .expect("authenticated status response");
    assert!(matches!(accepted, Response::Data(_)));
    let subscription = IPCClient::new(&socket_path)
        .with_auth_token("expected-token")
        .subscribe_status()
        .await
        .expect("authenticated status subscription");
    drop(subscription);

    server_task.abort();
    let _ = server_task.await;
}

#[cfg(feature = "ipc")]
#[tokio::test]
async fn active_ipc_socket_cannot_be_taken_over_by_a_second_server() {
    use hft_ipc::{IPCError, PreparedIPCListener};

    let directory = tempfile::tempdir().expect("temporary IPC directory");
    let socket_path = directory.path().join("single-owner.sock");
    let first = PreparedIPCListener::bind(&socket_path).expect("first server binds");

    let error = match PreparedIPCListener::bind(&socket_path) {
        Ok(second) => {
            drop(second);
            panic!("second server must not take over an active IPC socket");
        }
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            IPCError::Io(ref error) if error.kind() == std::io::ErrorKind::AddrInUse
        ),
        "unexpected takeover error: {error:?}"
    );
    assert!(
        tokio::net::UnixStream::connect(&socket_path).await.is_ok(),
        "the first server remains reachable after the rejected takeover"
    );

    drop(first);
    assert!(!socket_path.exists());
}

#[cfg(feature = "ipc")]
#[tokio::test]
async fn old_listener_drop_does_not_remove_a_replacement_socket_inode() {
    use hft_ipc::PreparedIPCListener;

    let directory = tempfile::tempdir().expect("temporary IPC directory");
    let socket_path = directory.path().join("inode-owned.sock");
    let first = PreparedIPCListener::bind(&socket_path).expect("first server binds");
    std::fs::remove_file(&socket_path).expect("simulate replacement of the socket path");
    let replacement = std::os::unix::net::UnixListener::bind(&socket_path)
        .expect("an uncoordinated replacement socket binds");

    drop(first);
    assert!(
        socket_path.exists(),
        "old listener must not unlink the replacement server's inode"
    );

    drop(replacement);
    std::fs::remove_file(&socket_path).expect("remove replacement socket");
    assert!(!socket_path.exists());
}

#[cfg(feature = "ipc")]
#[tokio::test]
async fn concurrent_stale_socket_claim_has_exactly_one_owner() {
    use hft_ipc::{IPCError, PreparedIPCListener};
    use std::sync::Arc;

    let directory = tempfile::tempdir().expect("temporary IPC directory");
    let socket_path = directory.path().join("concurrent-stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket_path).expect("create stale socket inode"));
    let start = Arc::new(tokio::sync::Barrier::new(2));
    let finish = Arc::new(tokio::sync::Barrier::new(2));
    let contender = |start: Arc<tokio::sync::Barrier>, finish: Arc<tokio::sync::Barrier>| {
        let socket_path = socket_path.clone();
        async move {
            start.wait().await;
            let result = PreparedIPCListener::bind(socket_path);
            finish.wait().await;
            result
        }
    };

    let (first, second) = tokio::join!(
        contender(Arc::clone(&start), Arc::clone(&finish)),
        contender(start, finish)
    );
    let outcomes = [first, second];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(
                result,
                Err(IPCError::Io(error)) if error.kind() == std::io::ErrorKind::AddrInUse
            ))
            .count(),
        1
    );
    assert!(
        tokio::net::UnixStream::connect(&socket_path).await.is_ok(),
        "the winning listener remains reachable"
    );

    drop(outcomes);
    assert!(!socket_path.exists());
}

#[test]
fn constants_available() {
    use hft_ipc::{DEFAULT_SOCKET_PATH, MAX_MESSAGE_SIZE};

    // Constants should always be available
    assert_eq!(DEFAULT_SOCKET_PATH, "/tmp/hft_control.sock");
    assert_eq!(MAX_MESSAGE_SIZE, 1024 * 1024);
}

#[test]
fn trading_mode_enum_available() {
    use hft_ipc::TradingMode;

    // Trading mode should always be available for configuration
    let modes = [
        TradingMode::Live,
        TradingMode::Paper,
        TradingMode::Replay,
        TradingMode::Paused,
    ];

    assert_eq!(modes.len(), 4);
}
