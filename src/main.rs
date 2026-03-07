// src/main.rs
mod network;
mod storage;

use crate::network::{MyBehaviourEvent, SyncMessage, SyncSession};
use crate::storage::FileTransferState;
use anyhow::Result;
use dialoguer::Input;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting Vitruvius...");

    let mut swarm = crate::network::setup_network().await?;

    // let mut currently_writing: HashSet<String> = HashSet::new();

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

    info!("Syncing folder: {:?}", sync_path_abs);
    if let Some(target) = target_peer_id {
        info!("Will sync with peer: {}", target);
    } else {
        info!("Waiting for incoming connections...");
    }

    // let (file_tx, mut file_rx) = mpsc::channel::<Event>(100);

    // let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
    //     if let Ok(event) = res {
    //         let _ = file_tx.blocking_send(event);
    //     }
    // })?;

    // watcher.watch(&sync_path_abs, RecursiveMode::Recursive)?;
    info!("Watching folder for changes");

    let mut transfer_states: HashMap<String, FileTransferState> = HashMap::new();
    let mut connected_peers: Vec<PeerId> = Vec::new();

    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, addr) in list {
                            info!("Discovered peer: {} at {}", peer_id, addr);

                            if let Some(target) = target_peer_id {
                                if peer_id == target {
                                    info!("Target peer found, dialing...");
                                    swarm.dial(addr)?;
                                }
                            }
                        }
                    }

                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        info!("Connected to {}", peer_id);

                        if !connected_peers.contains(&peer_id) {
                            connected_peers.push(peer_id);
                        }

                        if let Some(target) = target_peer_id {
                            if peer_id == target {
                                info!("Requesting file list...");
                                swarm.behaviour_mut().rr.send_request(
                                    &peer_id,
                                    SyncMessage::ListFiles,
                                );
                            }
                        }
                    }

                    SwarmEvent::Behaviour(MyBehaviourEvent::Rr(
                        request_response::Event::Message { peer, message }
                    )) => {
                        match message {
                            request_response::Message::Request { channel, request, .. } => {
                                match request {
                                    SyncMessage::ListFiles => {
                                        info!("Peer requested file list");

                                        let file_names = crate::storage::get_file_list(&sync_path_abs).await?;

                                        let _ = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::FileList { file_names }
                                        );
                                    }

                                    SyncMessage::Request { file_name } => {
                                        info!("Received metadata request for: {}", file_name);

                                        let response = crate::storage::get_file_metadata(
                                            &sync_path_abs,
                                            &file_name
                                        ).await?;
                                        let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                                    }

                                    SyncMessage::ChunkRequest { file_name, chunk_index } => {
                                        info!("Received chunk request {} for: {}", chunk_index, file_name);

                                        let response = crate::storage::get_chunk(
                                            &sync_path_abs,
                                            &file_name,
                                            chunk_index
                                        ).await?;
                                        let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                                    }

                                    SyncMessage::FileChanged { file_name } => {
                                        info!("Peer notified file changed: {}", file_name);

                                        swarm.behaviour_mut().rr.send_request(
                                            &peer,
                                            SyncMessage::Request { file_name }
                                        );

                                        let _ = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::TransferComplete
                                        );
                                    }

                                    _ => {
                                        warn!("Unexpected request type from {}", peer);
                                    }
                                }
                            }

                            request_response::Message::Response { response, .. } => {
                                match response {
                                    SyncMessage::FileList { file_names } => {
                                        info!("Received file list with {} files", file_names.len());


                                        info!("Creating a new Sync Sesion with all the files");

                                        let  mut sync_session : SyncSession = SyncSession::new(file_names);

                                        while sync_session.can_start_more() {

                                            if let Some(file) = sync_session.








                                        }



                                    }

                                    SyncMessage::Metadata { file_name, total_chunks, file_size, chunk_hashes } => {
                                        info!("Received metadata for '{}' ({} chunks, {} bytes)",
                                              file_name, total_chunks, file_size);

                                        let metadata = crate::storage::FileMetadata {
                                            file_name: file_name.clone(),
                                            total_chunks,
                                            file_size,
                                            chunk_hashes,
                                        };

                                        let mut transfer_state = FileTransferState::new(sync_path_abs.clone());
                                        transfer_state.metadata = Some(metadata);
                                        transfer_states.insert(file_name.clone(), transfer_state);

                                        for i in 0..total_chunks {
                                            swarm.behaviour_mut().rr.send_request(
                                                &peer,
                                                SyncMessage::ChunkRequest {
                                                    file_name: file_name.clone(),
                                                    chunk_index: i
                                                }
                                            );
                                        }
                                    }

                                    SyncMessage::ChunkResponse { chunk_index, data, hash,file_name } => {



                                        let mut transfer_needed = transfer_states.get_mut(&file_name).unwrap();

                                        if let Some(metadata) = &transfer_needed.metadata {

                                                if transfer_needed.received_chunks.len() < metadata.total_chunks{
                                                    match crate::storage::process_received_chunk(
                                                        peer,
                                                        chunk_index,
                                                        data.clone(),
                                                        hash,
                                                        &mut transfer_needed
                                                    ).await {
                                                        Ok(complete) => {
                                                            if complete {
                                                                // currently_writing.insert(file_name.clone());
                                                               tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                                                                // currently_writing.remove(file_name);

                                                                info!("Transfer complete for: {}", file_name);
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!("Failed to process chunk {}: {}", chunk_index, e);
                                                        }
                                                    }
                                                    break;


                                        }
                                        }

                                    }

                                    SyncMessage::Empty => {
                                        info!("Remote folder is empty");
                                    }

                                    SyncMessage::Error { message } => {
                                        error!("Error from {}: {}", peer, message);
                                    }

                                    _ => {
                                        warn!("Unexpected response type from {}", peer);
                                    }
                                }
                            }
                        }
                    }

                    SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                        warn!("Connection failed to {:?}: {}", peer_id, error);
                    }

                    _ => {}
                }
            }

            // Some(file_event) = file_rx.recv() => {
                // match file_event.kind {
                    // EventKind::Create(_) | EventKind::Modify(_) => {
                        // for path in file_event.paths {
                            // if path.is_file() {
                                // if let Some(file_name) = path.file_name() {
                                    // let file_name_str = file_name.to_str().unwrap().to_string();

                                    // if currently_writing.contains(&file_name_str) {
                                        // continue;
                                    // }
                                    // info!("File changed: {}", file_name_str);

                                    // for peer in &connected_peers {
                                        // swarm.behaviour_mut().rr.send_request(
                                            // peer,
                                            // SyncMessage::FileChanged {
                                                // file_name: file_name_str.clone()
                                            // }
                                        // );
                                    // }
                                // }
                            // }
                        // }
                    // }
                    // _ => {}
                // }
            // }

            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down...");
                break;
            }
        }
    }

    Ok(())
}
