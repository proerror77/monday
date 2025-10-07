//! IPC Server implementation

use crate::handlers::CommandHandler;
use crate::{Command, IPCError, IPCMessage, IPCPayload, IPCResult, Response, MAX_MESSAGE_SIZE};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// IPC Server for handling control plane commands
pub struct IPCServer<H: CommandHandler> {
    socket_path: String,
    handler: Arc<H>,
    status_tx: broadcast::Sender<IPCMessage>,
    shutdown_tx: broadcast::Sender<()>,
    auth_token: Option<String>,
}

impl<H: CommandHandler + Send + Sync + 'static> IPCServer<H> {
    /// Create new IPC server
    pub fn new<P: AsRef<Path>>(socket_path: P, handler: H) -> Self {
        Self::new_with_auth(socket_path, handler, None)
    }

    /// Create new IPC server with authentication token
    pub fn new_with_auth<P: AsRef<Path>>(
        socket_path: P,
        handler: H,
        auth_token: Option<String>,
    ) -> Self {
        let (status_tx, _) = broadcast::channel(1024);
        let (shutdown_tx, _) = broadcast::channel(1);

        Self {
            socket_path: socket_path.as_ref().to_string_lossy().to_string(),
            handler: Arc::new(handler),
            status_tx,
            shutdown_tx,
            auth_token,
        }
    }

    /// Start the IPC server
    pub async fn start(&self) -> IPCResult<()> {
        // Remove existing socket file if it exists
        if std::path::Path::new(&self.socket_path).exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&self.socket_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Set socket permissions to 0o600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&self.socket_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&self.socket_path, perms)?;
            info!("IPC socket permissions set to 0600");
        }

        info!("IPC server listening on {} (secure mode)", self.socket_path);

        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let handler = Arc::clone(&self.handler);
                            let status_tx = self.status_tx.clone();
                            let auth_token = self.auth_token.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_client(stream, handler, status_tx, auth_token).await {
                                    error!("Error handling client: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("IPC server shutting down");
                    break;
                }
            }
        }

        // Clean up socket file
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }

    /// Shutdown the server
    pub fn shutdown(&self) -> IPCResult<()> {
        self.shutdown_tx
            .send(())
            .map_err(|_| IPCError::Handler("Failed to send shutdown signal".to_string()))?;
        Ok(())
    }

    /// Broadcast status update to all connected clients
    pub fn broadcast_status(&self, message: IPCMessage) -> IPCResult<()> {
        self.status_tx
            .send(message)
            .map_err(|_| IPCError::Handler("No active clients to broadcast to".to_string()))?;
        Ok(())
    }

    /// Handle individual client connection
    async fn handle_client(
        mut stream: UnixStream,
        handler: Arc<H>,
        status_tx: broadcast::Sender<IPCMessage>,
        auth_token: Option<String>,
    ) -> IPCResult<()> {
        debug!("New client connected");

        // Subscribe to status updates
        let mut status_rx = status_tx.subscribe();

        loop {
            tokio::select! {
                // Handle incoming command from client
                result = Self::read_message(&mut stream) => {
                    match result {
                        Ok(Some(message)) => {
                            if let IPCPayload::Command(command) = message.payload {
                                debug!("Received command: {:?}", command);

                                // Token authentication (if required)
                                if let Some(ref required_token) = auth_token {
                                    // For now, check env var HFT_IPC_TOKEN for client auth
                                    if let Ok(client_token) = std::env::var("HFT_IPC_TOKEN") {
                                        if client_token != *required_token {
                                            warn!("Client authentication failed: invalid token");
                                            let response = Response::Error {
                                                message: "Authentication failed".to_string(),
                                                code: Some(401),
                                            };
                                            let response_msg = IPCMessage {
                                                id: message.id,
                                                timestamp: std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap()
                                                    .as_nanos() as u64,
                                                payload: IPCPayload::Response(response),
                                            };
                                            let _ = Self::write_message(&mut stream, &response_msg).await;
                                            break; // Disconnect client
                                        }
                                    } else {
                                        warn!("Client authentication failed: no token provided");
                                        break; // Disconnect client
                                    }
                                }

                                // Process command
                                let response = handler.handle_command(command).await;

                                // Send response
                                let response_msg = IPCMessage {
                                    id: message.id, // Use same ID for correlation
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_nanos() as u64,
                                    payload: IPCPayload::Response(response),
                                };

                                if let Err(e) = Self::write_message(&mut stream, &response_msg).await {
                                    error!("Failed to send response: {}", e);
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            debug!("Client disconnected");
                            break;
                        }
                        Err(e) => {
                            error!("Error reading message: {}", e);
                            break;
                        }
                    }
                }
                // Forward status updates to client
                result = status_rx.recv() => {
                    match result {
                        Ok(status_message) => {
                            if let Err(e) = Self::write_message(&mut stream, &status_message).await {
                                error!("Failed to send status update: {}", e);
                                break;
                            }
                        }
                        Err(_) => {
                            // Channel closed, exit
                            break;
                        }
                    }
                }
            }
        }

        debug!("Client connection closed");
        Ok(())
    }

    /// Read a message from the stream
    async fn read_message(stream: &mut UnixStream) -> IPCResult<Option<IPCMessage>> {
        // Read message length (4 bytes, big endian)
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None); // Client disconnected
            }
            Err(e) => return Err(e.into()),
        }

        let message_len = u32::from_be_bytes(len_buf) as usize;

        if message_len > MAX_MESSAGE_SIZE {
            return Err(IPCError::Handler(format!(
                "Message too large: {} bytes",
                message_len
            )));
        }

        // Read message data
        let mut message_buf = vec![0u8; message_len];
        stream.read_exact(&mut message_buf).await?;

        // Deserialize message
        let message: IPCMessage = rmp_serde::from_slice(&message_buf)?;
        Ok(Some(message))
    }

    /// Write a message to the stream
    async fn write_message(stream: &mut UnixStream, message: &IPCMessage) -> IPCResult<()> {
        // Serialize message
        let message_data = rmp_serde::to_vec(message)?;

        if message_data.len() > MAX_MESSAGE_SIZE {
            return Err(IPCError::Handler(format!(
                "Response too large: {} bytes",
                message_data.len()
            )));
        }

        // Write length prefix (4 bytes, big endian)
        let len_bytes = (message_data.len() as u32).to_be_bytes();
        stream.write_all(&len_bytes).await?;

        // Write message data
        stream.write_all(&message_data).await?;
        stream.flush().await?;

        Ok(())
    }
}
