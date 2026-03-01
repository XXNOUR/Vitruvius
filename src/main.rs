// src/main.rs
mod network;
mod storage;
mod sync;

use anyhow::Result;
use dialoguer::Input;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::network::{MyBehaviourEvent, SyncMessage};

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
                                info!("Sending file request...");
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
                            request_response::Message::Request { channel, .. } => {
                                info!(" Received file request from {}", peer);

                                // Prepare file (chunks it internally)
                                let response = crate::storage::prepare_file_send(
                                    peer,
                                    &sync_path_abs
                                ).await?;

                                // Send single response with all chunks
                                let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                            }

                            // Incoming response - we're the receiver
                            request_response::Message::Response { response, .. } => {
                                match response {
                                    SyncMessage::FileTransfer {
                                        file_name,
                                        chunks,
                                        file_size,
                                        total_chunks: _
                                    } => {
                                        // Receive and verify all chunks
                                        crate::storage::receive_file_transfer(
                                            peer,
                                            file_name,
                                            chunks,
                                            file_size,
                                            &sync_path_abs,
                                        ).await?;
                                    }

                                    SyncMessage::Empty => {
                                        info!("Remote folder is empty");
                                    }

                                    SyncMessage::Request { .. } => {
                                        warn!("  Unexpected Request in response");
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
                info!(" Shutting down...");
                break;
            }
        }
    }

    Ok(())
}
