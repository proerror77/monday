//! Control plane IPC integration tests

use hft_ipc::{IPCServer, IPCClient, Command, Response, handlers::MockCommandHandler};
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::time::timeout;

#[tokio::test]
async fn test_basic_ipc_communication() {
    // Create temporary socket
    let temp_file = NamedTempFile::new().unwrap();
    let socket_path = temp_file.path().to_str().unwrap();
    std::fs::remove_file(socket_path).unwrap(); // Remove file, keep path

    // Start server
    let server = IPCServer::new(socket_path, MockCommandHandler);
    let server_handle = tokio::spawn(async move {
        server.start().await.unwrap();
    });

    // Wait for server to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create client and test commands
    let client = IPCClient::new(socket_path);

    // Test start command
    let response = timeout(Duration::from_secs(5), client.start()).await
        .expect("Timeout waiting for response")
        .expect("Failed to send start command");
    
    assert!(matches!(response, Response::Ok));

    // Test status command
    let response = timeout(Duration::from_secs(5), client.get_status()).await
        .expect("Timeout waiting for response")
        .expect("Failed to send status command");
    
    assert!(matches!(response, Response::Data(_)));

    // Test stop command
    let response = timeout(Duration::from_secs(5), client.stop()).await
        .expect("Timeout waiting for response")
        .expect("Failed to send stop command");
    
    assert!(matches!(response, Response::Ok));

    // Cleanup
    server_handle.abort();
}

#[tokio::test]
async fn test_model_loading() {
    let temp_file = NamedTempFile::new().unwrap();
    let socket_path = temp_file.path().to_str().unwrap();
    std::fs::remove_file(socket_path).unwrap();

    let server = IPCServer::new(socket_path, MockCommandHandler);
    let server_handle = tokio::spawn(async move {
        server.start().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = IPCClient::new(socket_path);

    // Test model loading
    let response = timeout(
        Duration::from_secs(5), 
        client.load_model(
            "/path/to/model.pt".to_string(), 
            "v1.2.3".to_string(), 
            Some("abc123".to_string())
        )
    ).await
        .expect("Timeout waiting for response")
        .expect("Failed to send load model command");
    
    assert!(matches!(response, Response::Ok));

    server_handle.abort();
}

#[tokio::test]
async fn test_client_timeout() {
    // Test timeout when server is not running
    let client = IPCClient::new("/tmp/nonexistent_socket.sock")
        .with_timeout(Duration::from_millis(100));

    let result = client.start().await;
    assert!(result.is_err());
}

#[tokio::test] 
async fn test_invalid_socket_path() {
    // Test with invalid socket path
    let client = IPCClient::new("/invalid/path/socket.sock");
    
    let result = client.start().await;
    assert!(result.is_err());
}