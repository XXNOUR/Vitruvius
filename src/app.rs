use crate::network::{MyBehaviourEvent, SyncMessage, SyncSession, setup_network};
use crate::storage::{FileTransferState, get_file_list, get_file_metadata, process_received_chunk};
use crate::sync::SyncState;
use anyhow::Result;
use dialoguer::Input;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const MAX_CONCURRENT_FILES: usize = 3;
const MAX_CONCURRENT_CHUNKS: usize = 8;

pub async fn run() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting Vitruvius...");

    let mut swarm = setup_network().await?;

    let mut sync_state = SyncState::new();
    let mut currently_writing: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Cache for chunked files on the serving side
    let mut cached_chunks: HashMap<String, (Vec<crate::storage::Chunk>, crate::storage::FileMetadata)> =
        HashMap::new();

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

    let (file_tx, mut file_rx) = mpsc::channel::<Event>(100);

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let _ = file_tx.blocking_send(event);
        }
    })?;

    watcher.watch(&sync_path_abs, RecursiveMode::Recursive)?;
    info!("Watching folder for changes");

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

                        if !sync_state.connected_peers.contains(&peer_id) {
                            sync_state.connected_peers.push(peer_id);
                        }

                        if let Some(target) = target_peer_id {
                            if peer_id == target {
                                sync_state.sync_target_peer = Some(peer_id);
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
                                        let file_names = get_file_list(&sync_path_abs).await?;

                                        if let Err(e) = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::FileList { file_names }
                                        ) {
                                            warn!("Failed to send file list response: {:?}", e);
                                        }
                                    }

                                    SyncMessage::MetaDataRequest { file_name } => {
                                        info!("Received metadata request for: {}", file_name);

                                        // Check cache first
                                        let response = if let Some((_, metadata)) = cached_chunks.get(&file_name) {
                                            SyncMessage::Metadata {
                                                file_name: metadata.file_name.clone(),
                                                total_chunks: metadata.total_chunks,
                                                file_size: metadata.file_size,
                                                chunk_hashes: metadata.chunk_hashes.clone(),
                                            }
                                        } else {
                                            match get_file_metadata(&sync_path_abs, &file_name).await {
                                                Ok(msg) => msg,
                                                Err(e) => {
                                                    SyncMessage::Error {
                                                        message: format!("Failed to get metadata: {}", e),
                                                    }
                                                }
                                            }
                                        };

                                        // Cache the chunks if metadata was successful
                                        if let SyncMessage::Metadata { ref file_name, .. } = response {
                                            if !cached_chunks.contains_key(file_name) {
                                                let file_path = sync_path_abs.join(file_name);
                                                if let Ok((chunks, metadata)) = crate::storage::chunk_file(&file_path) {
                                                    cached_chunks.insert(file_name.clone(), (chunks, metadata));
                                                }
                                            }
                                        }

                                        if let Err(e) = swarm.behaviour_mut().rr.send_response(channel, response) {
                                            warn!("Failed to send metadata response: {:?}", e);
                                        }
                                    }

                                    SyncMessage::ChunkRequest { file_name, chunk_index } => {
                                        info!("Received chunk request {} for: {}", chunk_index, file_name);

                                        let response = if let Some((chunks, _)) = cached_chunks.get(&file_name) {
                                            if let Some(chunk) = chunks.get(chunk_index) {
                                                SyncMessage::ChunkResponse {
                                                    chunk_index: chunk.index,
                                                    data: chunk.data.clone(),
                                                    hash: chunk.hash,
                                                    file_name: file_name.clone(),
                                                }
                                            } else {
                                                SyncMessage::Error {
                                                    message: format!("Chunk {} out of range", chunk_index),
                                                }
                                            }
                                        } else {
                                            // Fallback: load and chunk the file
                                            let file_path = sync_path_abs.join(&file_name);
                                            match crate::storage::chunk_file(&file_path) {
                                                Ok((chunks, metadata)) => {
                                                    cached_chunks.insert(file_name.clone(), (chunks.clone(), metadata));
                                                    if let Some(chunk) = chunks.get(chunk_index) {
                                                        SyncMessage::ChunkResponse {
                                                            chunk_index: chunk.index,
                                                            data: chunk.data.clone(),
                                                            hash: chunk.hash,
                                                            file_name: file_name.clone(),
                                                        }
                                                    } else {
                                                        SyncMessage::Error {
                                                            message: format!("Chunk {} out of range", chunk_index),
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    SyncMessage::Error {
                                                        message: format!("Failed to read file: {}", e),
                                                    }
                                                }
                                            }
                                        };

                                        if let Err(e) = swarm.behaviour_mut().rr.send_response(channel, response) {
                                            warn!("Failed to send chunk response: {:?}", e);
                                        }
                                    }

                                    SyncMessage::FileChanged { file_name } => {
                                        info!("Peer notified file changed: {}", file_name);

                                        // Clear cache for this file
                                        cached_chunks.remove(&file_name);

                                        // Request metadata to check if we need to update
                                        swarm.behaviour_mut().rr.send_request(
                                            &peer,
                                            SyncMessage::MetaDataRequest { file_name },
                                        );

                                        if let Err(e) = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::TransferComplete,
                                        ) {
                                            warn!("Failed to send TransferComplete response: {:?}", e);
                                        }
                                    }

                                    _ => {
                                        warn!("Unexpected request type from {}", peer);
                                        let _ = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::Error {
                                                message: "Unexpected request type".to_string(),
                                            }
                                        );
                                    }
                                }
                            }

                            request_response::Message::Response { response, .. } => {
                                match response {
                                    SyncMessage::FileList { file_names } => {
                                        info!("Received file list with {} files", file_names.len());
                                        info!("Creating a new Sync Session with all the files");

                                        if file_names.is_empty() {
                                            info!("Remote folder is empty, sync complete");
                                            sync_state.is_initial_sync_complete = true;
                                            continue;
                                        }

                                        let mut sync_session = SyncSession::new(file_names, MAX_CONCURRENT_FILES);

                                        // Start initial transfers (up to max concurrent)
                                        while sync_session.can_start_more() {
                                            if let Some(file_name) = sync_session.start_next() {
                                                info!("Requesting metadata for: {}", file_name);
                                                if let Some(target) = sync_state.sync_target_peer {
                                                    swarm.behaviour_mut().rr.send_request(
                                                        &target,
                                                        SyncMessage::MetaDataRequest { file_name },
                                                    );
                                                }
                                            }
                                        }

                                        sync_state.current_session = Some(sync_session);
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
                                        transfer_state.metadata = Some(metadata.clone());
                                        transfer_state.chunks_expected = total_chunks;
                                        sync_state.transfer_states.insert(file_name.clone(), transfer_state);

                                        // Request first batch of chunks (up to MAX_CONCURRENT_CHUNKS)
                                        let chunks_to_request = std::cmp::min(MAX_CONCURRENT_CHUNKS, total_chunks);
                                        for i in 0..chunks_to_request {
                                            if let Some(target) = sync_state.sync_target_peer {
                                                swarm.behaviour_mut().rr.send_request(
                                                    &target,
                                                    SyncMessage::ChunkRequest {
                                                        file_name: file_name.clone(),
                                                        chunk_index: i,
                                                    }
                                                );
                                            }
                                        }
                                    }

                                    SyncMessage::ChunkResponse { chunk_index, data, hash, file_name } => {
                                        let transfer_needed = match sync_state.transfer_states.get_mut(&file_name) {
                                            Some(state) => state,
                                            None => {
                                                info!("Received chunk for file '{}' but no active transfer found. Ignoring.", file_name);
                                                continue;
                                            }
                                        };

                                        if let Some(metadata) = &transfer_needed.metadata {
                                            let total_chunks = metadata.total_chunks;
                                            if transfer_needed.received_chunks.len() < total_chunks {
                                                match process_received_chunk(
                                                    peer,
                                                    chunk_index,
                                                    data.clone(),
                                                    hash,
                                                    transfer_needed,
                                                ).await {
                                                    Ok(complete) => {
                                                        if complete {
                                                            currently_writing.insert(file_name.clone());
                                                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                                                            currently_writing.remove(&file_name);

                                                            info!("Transfer complete for: {}", file_name);

                                                            sync_state.transfer_states.remove(&file_name);

                                                            // Clear from cache after transfer
                                                            cached_chunks.remove(&file_name);

                                                            if let Some(session) = &mut sync_state.current_session {
                                                                session.mark_complete(&file_name);

                                                                // Start next file if available
                                                                while session.can_start_more() {
                                                                    if let Some(next_file) = session.start_next() {
                                                                        info!("Starting next file: {}", next_file);
                                                                        if let Some(target) = sync_state.sync_target_peer {
                                                                            swarm.behaviour_mut().rr.send_request(
                                                                                &target,
                                                                                SyncMessage::MetaDataRequest { file_name: next_file },
                                                                            );
                                                                        }
                                                                    }
                                                                }

                                                                if session.is_done() {
                                                                    info!("All files synchronized successfully");
                                                                    sync_state.current_session = None;
                                                                    sync_state.is_initial_sync_complete = true;
                                                                }
                                                            }
                                                        } else {
                                                            let next_chunk_index = transfer_needed.next_chunk_to_request;
                                                            if next_chunk_index < total_chunks && transfer_needed.chunks_in_flight < MAX_CONCURRENT_CHUNKS {
                                                                transfer_needed.next_chunk_to_request += 1;
                                                                transfer_needed.chunks_in_flight += 1;
                                                                if let Some(target) = sync_state.sync_target_peer {
                                                                    swarm.behaviour_mut().rr.send_request(
                                                                        &target,
                                                                        SyncMessage::ChunkRequest {
                                                                            file_name: file_name.clone(),
                                                                            chunk_index: next_chunk_index,
                                                                        }
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!("Failed to process chunk {}: {}", chunk_index, e);
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    SyncMessage::TransferComplete => {
                                        info!("Peer acknowledged file change notification");
                                    }

                                    SyncMessage::Empty => {
                                        info!("Remote folder is empty");
                                        sync_state.is_initial_sync_complete = true;
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

                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        info!("Connection closed with {}", peer_id);
                        sync_state.connected_peers.retain(|p| p != &peer_id);
                        if sync_state.sync_target_peer == Some(peer_id) {
                            sync_state.sync_target_peer = None;
                            warn!("Sync target disconnected");
                        }
                    }

                    _ => {}
                }
            }

            Some(file_event) = file_rx.recv() => {
                if !sync_state.is_initial_sync_complete {
                    continue;
                }

                match file_event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in file_event.paths {
                            if path.is_file() {
                                if let Some(file_name) = path.file_name() {
                                    let file_name_str = file_name.to_str().unwrap().to_string();

                                    // Skip files we're currently writing
                                    if currently_writing.contains(&file_name_str) {
                                        continue;
                                    }

                                    // Skip temporary files
                                    if file_name_str.starts_with('.') || file_name_str.ends_with(".tmp") {
                                        continue;
                                    }

                                    info!("File changed: {}", file_name_str);

                                    // Invalidate cache for this file
                                    cached_chunks.remove(&file_name_str);

                                    for peer in &sync_state.connected_peers {
                                        swarm.behaviour_mut().rr.send_request(
                                            peer,
                                            SyncMessage::FileChanged {
                                                file_name: file_name_str.clone(),
                                            }
                                        );
                                    }
                                }
                            }
                        }
                    }
                    EventKind::Remove(_) => {
                        for path in file_event.paths {
                            if let Some(file_name) = path.file_name() {
                                let file_name_str = file_name.to_str().unwrap().to_string();

                                info!("File removed: {}", file_name_str);

                                // Remove from cache
                                cached_chunks.remove(&file_name_str);

                                // Notify peers about deletion
                                for peer in &sync_state.connected_peers {
                                    swarm.behaviour_mut().rr.send_request(
                                        peer,
                                        SyncMessage::FileDeleted {
                                            file_name: file_name_str.clone(),
                                        }
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down...");
                break;
            }
        }
    }

    Ok(())
}
