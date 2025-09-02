//! IPC Client implementation

use crate::{IPCMessage, IPCPayload, Command, Response, ResponseData, StatusUpdate, IPCError, IPCResult, MAX_MESSAGE_SIZE};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use std::collections::HashMap;
use uuid::Uuid;
use tracing::{debug, error, warn};

/// IPC Client for sending commands to the control plane
pub struct IPCClient {
    socket_path: String,
    default_timeout: Duration,
}

impl IPCClient {
    /// Create new IPC client
    pub fn new<P: AsRef<Path>>(socket_path: P) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_string_lossy().to_string(),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Set default timeout for commands
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Send a command and wait for response
    pub async fn send_command(&self, command: Command) -> IPCResult<Response> {
        self.send_command_with_timeout(command, self.default_timeout).await
    }

    /// Send a command with custom timeout
    pub async fn send_command_with_timeout(
        &self, 
        command: Command, 
        timeout_duration: Duration
    ) -> IPCResult<Response> {
        let result = timeout(timeout_duration, self.send_command_internal(command)).await;
        
        match result {
            Ok(response) => response,
            Err(_) => Err(IPCError::Timeout),
        }
    }

    /// Internal command sending logic
    async fn send_command_internal(&self, command: Command) -> IPCResult<Response> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        
        // Create message
        let message = IPCMessage::new(IPCPayload::Command(command));
        let message_id = message.id;
        
        // Send message
        Self::write_message(&mut stream, &message).await?;
        
        // Wait for response
        loop {
            if let Some(response_msg) = Self::read_message(&mut stream).await? {
                // Check if this is our response
                if response_msg.id == message_id {
                    if let IPCPayload::Response(response) = response_msg.payload {
                        return Ok(response);
                    }
                }
                // Skip status updates or unrelated messages
            } else {
                return Err(IPCError::Handler("Connection closed while waiting for response".to_string()));
            }
        }
    }

    /// Subscribe to status updates
    pub async fn subscribe_status(&self) -> IPCResult<StatusSubscription> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (status_tx, status_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        // Spawn task to handle incoming status updates
        tokio::spawn(async move {
            let mut stream = stream;
            loop {
                tokio::select! {
                    result = Self::read_message(&mut stream) => {
                        match result {
                            Ok(Some(message)) => {
                                if let IPCPayload::Status(status) = message.payload {
                                    if status_tx.send(status).is_err() {
                                        debug!("Status receiver dropped, closing subscription");
                                        break;
                                    }
                                }
                                // Skip command responses
                            }
                            Ok(None) => {
                                debug!("Server closed connection");
                                break;
                            }
                            Err(e) => {
                                error!("Error reading status update: {}", e);
                                break;
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        debug!("Status subscription shutdown requested");
                        break;
                    }
                }
            }
        });

        Ok(StatusSubscription {
            receiver: status_rx,
            _shutdown_tx: shutdown_tx,
        })
    }

    /// Read a message from the stream
    async fn read_message(stream: &mut UnixStream) -> IPCResult<Option<IPCMessage>> {
        // Read message length (4 bytes, big endian)
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None); // Server closed connection
            }
            Err(e) => return Err(e.into()),
        }

        let message_len = u32::from_be_bytes(len_buf) as usize;
        
        if message_len > MAX_MESSAGE_SIZE {
            return Err(IPCError::Handler(
                format!("Message too large: {} bytes", message_len)
            ));
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
            return Err(IPCError::Handler(
                format!("Message too large: {} bytes", message_data.len())
            ));
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

/// Convenience methods for common commands
impl IPCClient {
    /// Start the trading system
    pub async fn start(&self) -> IPCResult<Response> {
        self.send_command(Command::Start).await
    }

    /// Stop the trading system
    pub async fn stop(&self) -> IPCResult<Response> {
        self.send_command(Command::Stop).await
    }

    /// Emergency stop
    pub async fn emergency_stop(&self) -> IPCResult<Response> {
        self.send_command(Command::EmergencyStop).await
    }

    /// Load model
    pub async fn load_model(&self, model_path: String, model_version: String, sha256_hash: Option<String>) -> IPCResult<Response> {
        self.send_command(Command::LoadModel {
            model_path,
            model_version,
            sha256_hash,
        }).await
    }

    /// Get system status
    pub async fn get_status(&self) -> IPCResult<Response> {
        self.send_command(Command::GetStatus).await
    }

    /// Get account information
    pub async fn get_account(&self) -> IPCResult<Response> {
        self.send_command(Command::GetAccount).await
    }

    /// Get positions
    pub async fn get_positions(&self) -> IPCResult<Response> {
        self.send_command(Command::GetPositions).await
    }

    /// Get open orders
    pub async fn get_open_orders(&self) -> IPCResult<Response> {
        self.send_command(Command::GetOpenOrders).await
    }

    /// Cancel all orders
    pub async fn cancel_all_orders(&self) -> IPCResult<Response> {
        self.send_command(Command::CancelAllOrders).await
    }
}

/// Status update subscription
pub struct StatusSubscription {
    receiver: mpsc::UnboundedReceiver<StatusUpdate>,
    _shutdown_tx: oneshot::Sender<()>, // Dropped when subscription is dropped
}

impl StatusSubscription {
    /// Receive next status update
    pub async fn recv(&mut self) -> Option<StatusUpdate> {
        self.receiver.recv().await
    }

    /// Try to receive status update without blocking
    pub fn try_recv(&mut self) -> Result<StatusUpdate, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}