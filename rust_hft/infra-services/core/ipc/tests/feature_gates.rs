//! Feature gate integration tests
//!
//! These tests verify that the crate can be used in different feature configurations.

use hft_ipc::{Command, Response, IPCPayload, IPCMessage};

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
    let _phantom: fn() -> IPCClient = || {
        unreachable!("This is never called, just for type checking")
    };
}

#[cfg(feature = "ipc")]
#[test]
fn request_id_is_uuid_with_feature() {
    use hft_ipc::RequestId;

    // With IPC feature, RequestId should be UUID
    let id: RequestId = uuid::Uuid::new_v4();
    assert!(id.to_string().len() > 0);
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
            code: Some(500)
        },
    ];

    assert_eq!(responses.len(), 2);
}

#[cfg(feature = "ipc")]
#[test]
fn serialization_roundtrip_with_messagepack() {
    use rmp_serde::{encode, decode};

    // With IPC feature, MessagePack serialization should work
    let command = Command::GetStatus;
    let payload = IPCPayload::Command(command.clone());
    let message = IPCMessage::new(payload);

    // Serialize
    let bytes = encode::to_vec(&message).expect("Failed to serialize");
    assert!(bytes.len() > 0);

    // Deserialize
    let decoded: IPCMessage = decode::from_slice(&bytes).expect("Failed to deserialize");

    match decoded.payload {
        IPCPayload::Command(Command::GetStatus) => {},
        _ => panic!("Wrong payload type after roundtrip"),
    }
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
