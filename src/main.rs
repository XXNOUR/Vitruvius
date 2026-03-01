// src/main.rs
mod network;
mod storage;

use anyhow::Result;
use dialoguer::Input;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info, warn};

use crate::network::{MyBehaviourEvent, SyncMessage};
use crate::storage::FileTransferState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting Vitruvius...");

    // Setup network first
    let mut swarm = crate::network::setup_network().await?;

    // Get user input BEFORE entering event loop
    let target_id_str: String = Input::new()
        .with_prompt("Enter Peer ID to sync with (or leave empty to wait)")
        .allow_empty(true)
        .interact_text()?;
    let target_peer_id = target_id_str.parse::<PeerId>().ok();

    let sync_path_input: String = Input::new()
        .with_prompt("Enter folder path")
        .interact_text()?;
    let sync_path = PathBuf::from(&sync_path_input);
    if !sync_path.exists() {
        fs::create_dir_all(&sync_path)?;
    }
    let sync_path_abs = fs::canonicalize(&sync_path)?;

    info!("✓ Syncing folder: {:?}", sync_path_abs);
    if let Some(target) = target_peer_id {
        info!("✓ Will sync with peer: {}", target);
    } else {
        info!("✓ Waiting for incoming connections...");
    }

    // Track ongoing transfers
    let mut transfer_states: HashMap<PeerId, FileTransferState> = HashMap::new();

    // Event loop
    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    // mDNS peer discovery
                    SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, addr) in list {
                            info!("Discovered peer: {} at {}", peer_id, addr);

                            // If we're looking for a specific peer, dial them
                            if let Some(target) = target_peer_id {
                                if peer_id == target {
                                    info!("✓ Target peer found! Dialing...");
                                    swarm.dial(addr)?;
                                }
                            }
                        }
                    }

                    // Connection established
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        info!("Connected to {}", peer_id);

                        // If we initiated the connection, send a request
                        if let Some(target) = target_peer_id {
                            if peer_id == target {
                                info!("Requesting file metadata...");
                                swarm.behaviour_mut().rr.send_request(
                                    &peer_id,
                                    SyncMessage::Request {
                                        file_name: "MANIFEST_REQ".into(),
                                    },
                                );
                            }
                        }
                    }

                    // Request-Response messages
                    SwarmEvent::Behaviour(MyBehaviourEvent::Rr(
                        request_response::Event::Message { peer, message }
                    )) => {
                        match message {
                            // Incoming request - we're the sender
                            request_response::Message::Request { channel, request,.. } => {
                                match request {
                                    SyncMessage::Request { file_name } => {
                                        info!("📥 Received file request from {}", peer);

                                        // Send metadata
                                        let response = crate::storage::get_file_metadata(
                                            &sync_path_abs
                                        ).await?;
                                        let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                                    }

                                    SyncMessage::ChunkRequest { chunk_index } => {
                                        info!("📦 Received chunk request {} from {}", chunk_index, peer);

                                        // Send specific chunk
                                        let response = crate::storage::get_chunk(
                                            &sync_path_abs,
                                            chunk_index
                                        ).await?;
                                        let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                                    }

                                    _ => {
                                        warn!("🚫 Unexpected request type from {}", peer);
                                    }
                                }
                            }

                            // Incoming response - we're the receiver
                            request_response::Message::Response { response, .. } => {
                                match response {
                                    SyncMessage::Metadata { file_name, total_chunks, file_size, chunk_hashes } => {
                                        info!("📋 Received metadata for '{}' ({} chunks, {} bytes)",
                                              file_name, total_chunks, file_size);

                                        // Initialize transfer state
                                        let metadata = crate::storage::FileMetadata {
                                            file_name,
                                            total_chunks,
                                            file_size,
                                            chunk_hashes,
                                        };

                                        let mut transfer_state = FileTransferState::new(sync_path_abs.clone());
                                        transfer_state.metadata = Some(metadata);
                                        transfer_states.insert(peer, transfer_state);

                                        // Start requesting chunks
                                        for i in 0..total_chunks {
                                            swarm.behaviour_mut().rr.send_request(
                                                &peer,
                                                SyncMessage::ChunkRequest { chunk_index: i }
                                            );
                                        }
                                    }

                                    SyncMessage::ChunkResponse { chunk_index, data, hash } => {
                                        if let Some(transfer_state) = transfer_states.get_mut(&peer) {
                                            match crate::storage::process_received_chunk(
                                                peer,
                                                chunk_index,
                                                data,
                                                hash,
                                                transfer_state
                                            ).await {
                                                Ok(complete) => {
                                                    if complete {
                                                        // Send completion acknowledgment
                                                        swarm.behaviour_mut().rr.send_request(
                                                            &peer,
                                                            SyncMessage::TransferComplete
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Failed to process chunk {}: {}", chunk_index, e);
                                                }
                                            }
                                        }
                                    }

                                    SyncMessage::Empty => {
                                        info!("📭 Remote folder is empty");
                                    }

                                    SyncMessage::Error { message } => {
                                        error!("❌ Error from {}: {}", peer, message);
                                    }

                                    _ => {
                                        warn!("🚫 Unexpected response type from {}", peer);
                                    }
                                }
                            }
                        }
                    }

                    // Connection errors
                    SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                        warn!("Connection failed to {:?}: {}", peer_id, error);
                    }

                    _ => {}
                }
            }

            // Handle Ctrl+C gracefully
            _ = tokio::signal::ctrl_c() => {
                info!("🛑 Shutting down...");
                break;
            }
        }
    }

    Ok(())
}
