use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use futures_util::stream::StreamExt;
use futures_util::sink::SinkExt;
use tracing::{info, warn};

/// Messages from the GUI to the backend
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GuiCommand {
    #[serde(rename = "DialPeer")]
    DialPeer { peer_id: String },
    #[serde(rename = "DialAddr")]
    DialAddr { addr: String },
    #[serde(rename = "Disconnect")]
    Disconnect { peer_id: String },
    #[serde(rename = "SetFolder")]
    SetFolder { path: String },
    #[serde(rename = "RequestSync")]
    RequestSync { peer_id: String },
    #[serde(rename = "GetFileList")]
    GetFileList,
    #[serde(rename = "GetFileHistory")]
    GetFileHistory { file_name: String },
}

/// Messages from the backend to the GUI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GuiEvent {
    #[serde(rename = "Identity")]
    Identity { peer_id: String, node_name: String },
    #[serde(rename = "PeerDiscovered")]
    PeerDiscovered { peer_id: String, addr: String, node_name: Option<String> },
    #[serde(rename = "PeerConnected")]
    PeerConnected { peer_id: String, node_name: Option<String> },
    #[serde(rename = "PeerDisconnected")]
    PeerDisconnected { peer_id: String },
    #[serde(rename = "TransferStarted")]
    TransferStarted {
        peer_id: String,
        file_name: String,
        total_chunks: usize,
        file_size: u64,
    },
    #[serde(rename = "ChunkReceived")]
    ChunkReceived {
        peer_id: String,
        file_name: String,
        chunk_index: usize,
        verified: bool,
    },
    #[serde(rename = "TransferComplete")]
    TransferComplete { peer_id: String, file_name: String },
    #[serde(rename = "FileList")]
    FileList { files: Vec<FileInfo> },
    #[serde(rename = "FileHistory")]
    FileHistory { file_name: String, versions: Vec<FileVersionInfo> },
    #[serde(rename = "Log")]
    Log { level: String, message: String },
    #[serde(rename = "Error")]
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
}

/// Information about a single version of a file for display in GUI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersionInfo {
    /// Hex-encoded file hash (first 16 chars for display)
    pub file_hash: String,
    /// Timestamp when version was created (unix seconds)
    pub timestamp: u64,
    /// Author/peer that created this version
    pub author: String,
    /// Number of chunks that changed from parent
    pub changed_chunks: usize,
    /// Total chunks in this version
    pub total_chunks: usize,
}

pub struct WsServer {
    clients: Arc<RwLock<HashMap<String, tokio::sync::mpsc::UnboundedSender<Message>>>>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<GuiEvent>>>,
    command_tx: mpsc::UnboundedSender<GuiCommand>,
}

impl WsServer {
    pub fn new(command_tx: mpsc::UnboundedSender<GuiCommand>) -> (Self, mpsc::UnboundedSender<GuiEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        (
            WsServer {
                clients: Arc::new(RwLock::new(HashMap::new())),
                event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
                command_tx,
            },
            event_tx,
        )
    }

    pub async fn start(&self) -> Result<()> {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        info!("WebSocket server listening on ws://0.0.0.0:8080");

        let clients = self.clients.clone();
        let event_rx = self.event_rx.clone();
        let command_tx = self.command_tx.clone();

        // Spawn event broadcaster task
        let clients_broadcast = clients.clone();
        tokio::spawn(async move {
            let mut rx = event_rx.lock().await;
            while let Some(event) = rx.recv().await {
                if let Ok(msg_text) = serde_json::to_string(&event) {
                    let msg = Message::Text(msg_text);
                    let mut clients_lock = clients_broadcast.write().await;
                    let mut dead_clients = Vec::new();

                    for (id, tx) in clients_lock.iter() {
                        if tx.send(msg.clone()).is_err() {
                            dead_clients.push(id.clone());
                        }
                    }

                    for id in dead_clients {
                        clients_lock.remove(&id);
                    }
                }
            }
        });

        // Accept connections
        loop {
            let (stream, addr) = listener.accept().await?;
            let clients = clients.clone();
            let command_tx = command_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_ws_connection(stream, addr, clients, command_tx).await {
                    warn!("WebSocket error: {}", e);
                }
            });
        }
    }
}

async fn handle_ws_connection(
    stream: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
    clients: Arc<RwLock<HashMap<String, tokio::sync::mpsc::UnboundedSender<Message>>>>,
    command_tx: mpsc::UnboundedSender<GuiCommand>,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let client_id = format!("{}", addr);
    info!("WebSocket client connected: {}", client_id);

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    {
        let mut clients = clients.write().await;
        clients.insert(client_id.clone(), tx);
    }

    let client_id_clone = client_id.clone();
    let clients_clone = clients.clone();

    // Handle incoming messages
    let recv_task = tokio::spawn(async move {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(cmd) = serde_json::from_str::<GuiCommand>(&text) {
                        info!("GUI command received: {:?}", cmd);
                        // Forward to app command handler
                        if let Err(_) = command_tx.send(cmd) {
                            warn!("Failed to send command to app - app channel closed");
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        let mut clients = clients_clone.write().await;
        clients.remove(&client_id_clone);
        info!("WebSocket client disconnected: {}", client_id_clone);
    });

    // Handle outgoing messages
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_write.send(msg).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = recv_task => {},
        _ = send_task => {},
    }

    Ok(())
}

pub fn log_to_gui(level: &str, message: &str) -> GuiEvent {
    GuiEvent::Log {
        level: level.to_string(),
        message: message.to_string(),
    }
}
