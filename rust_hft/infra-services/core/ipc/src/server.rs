//! IPC Server implementation

use crate::handlers::CommandHandler;
use crate::{IPCError, IPCMessage, IPCPayload, IPCResult, Response, MAX_MESSAGE_SIZE};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// A bound IPC listener whose socket permissions and owner have been verified.
pub struct PreparedIPCListener {
    listener: UnixListener,
    _lock_file: std::fs::File,
    owner_uid: u32,
    socket_dev: u64,
    socket_ino: u64,
    socket_path: PathBuf,
}

impl PreparedIPCListener {
    /// Bind and secure a Unix listener without starting command handling.
    pub fn bind<P: AsRef<Path>>(socket_path: P) -> IPCResult<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut lock_name = socket_path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&lock_path)?;
        let process_uid = unsafe { libc::geteuid() };
        let lock_metadata = lock_file.metadata()?;
        if !lock_metadata.file_type().is_file() || lock_metadata.uid() != process_uid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "IPC lock file is not a regular file owned by this user",
            )
            .into());
        }
        let mut lock_permissions = lock_metadata.permissions();
        lock_permissions.set_mode(0o600);
        lock_file.set_permissions(lock_permissions)?;
        fs4::FileExt::try_lock(&lock_file).map_err(|error| {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "another Monday process owns the IPC control socket lock",
                )
            } else {
                error
            }
        })?;

        match std::fs::symlink_metadata(&socket_path) {
            Ok(existing) => {
                if !existing.file_type().is_socket() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "IPC path exists and is not a Unix socket",
                    )
                    .into());
                }
                if existing.uid() != process_uid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "refusing to remove an IPC socket owned by another user",
                    )
                    .into());
                }
                match std::os::unix::net::UnixStream::connect(&socket_path) {
                    Ok(_) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AddrInUse,
                            "an active IPC server already owns this socket",
                        )
                        .into())
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                        let current = std::fs::symlink_metadata(&socket_path)?;
                        if !current.file_type().is_socket()
                            || current.uid() != existing.uid()
                            || current.dev() != existing.dev()
                            || current.ino() != existing.ino()
                        {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::AddrInUse,
                                "IPC socket changed while checking whether it was stale",
                            )
                            .into());
                        }
                        std::fs::remove_file(&socket_path)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let listener = UnixListener::bind(&socket_path)?;

        let metadata = std::fs::symlink_metadata(&socket_path)?;
        if !metadata.file_type().is_socket() {
            return Err(IPCError::Handler(
                "IPC path is not the socket that was just bound".to_string(),
            ));
        }
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        let prepared = Self {
            listener,
            _lock_file: lock_file,
            owner_uid: metadata.uid(),
            socket_dev: metadata.dev(),
            socket_ino: metadata.ino(),
            socket_path,
        };
        std::fs::set_permissions(&prepared.socket_path, perms)?;
        info!("IPC socket permissions set to 0600");

        Ok(prepared)
    }
}

impl Drop for PreparedIPCListener {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let owns_socket_path = std::fs::symlink_metadata(&self.socket_path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.uid() == self.owner_uid
                && metadata.dev() == self.socket_dev
                && metadata.ino() == self.socket_ino
        });
        if owns_socket_path {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

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

    /// Bind and secure the Unix socket before the server is advertised as ready.
    pub fn prepare(&self) -> IPCResult<PreparedIPCListener> {
        PreparedIPCListener::bind(&self.socket_path)
    }

    /// Start the IPC server.
    pub async fn start(&self) -> IPCResult<()> {
        let prepared = self.prepare()?;
        self.serve(prepared).await
    }

    /// Serve a listener that has already been bound and secured by [`Self::prepare`].
    pub async fn serve(&self, prepared: PreparedIPCListener) -> IPCResult<()> {
        if prepared.socket_path.as_path() != Path::new(&self.socket_path) {
            return Err(IPCError::Handler(
                "prepared IPC listener path does not match server path".to_string(),
            ));
        }

        info!("IPC server listening on {} (secure mode)", self.socket_path);

        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                result = prepared.listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let peer_uid = match stream.peer_cred() {
                                Ok(credentials) => credentials.uid(),
                                Err(error) => {
                                    warn!(%error, "Rejecting IPC connection with unavailable peer credentials");
                                    continue;
                                }
                            };
                            if peer_uid != prepared.owner_uid {
                                warn!(peer_uid, owner_uid = prepared.owner_uid, "Rejecting IPC connection from a different user");
                                continue;
                            }
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
        let mut authenticated = auth_token.is_none();

        loop {
            tokio::select! {
                // Handle incoming command from client
                result = Self::read_message(&mut stream) => {
                    match result {
                        Ok(Some(message)) => {
                            if let IPCPayload::Command(command) = message.payload {
                                debug!("Received command: {:?}", command);

                                // Token authentication (if required). The token travels only in the
                                // request envelope and is never included in logs or responses.
                                if auth_token.as_deref().is_some_and(|required_token| {
                                    message.auth_token.as_deref() != Some(required_token)
                                }) {
                                    warn!("Client authentication failed");
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
                                        auth_token: None,
                                    };
                                    let _ = Self::write_message(&mut stream, &response_msg).await;
                                    break;
                                }
                                authenticated = true;

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
                                    auth_token: None,
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
                            if !authenticated {
                                continue;
                            }
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
