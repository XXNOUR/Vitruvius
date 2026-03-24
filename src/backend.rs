use crate::network::{MyBehaviourEvent, SyncMessage, SyncSession, setup_network};
use crate::storage::{FileTransferState, get_file_list, get_file_metadata, process_received_chunk};
use crate::sync::SyncState;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const MAX_CONCURRENT_FILES: usize = 3;
const MAX_CONCURRENT_CHUNKS: usize = 8;

/// Commands sent from the GUI to the backend.
#[derive(Debug)]
pub enum UiCommand {
    StartSync {
        peer_id_str: String,
        folder_path: String,
    },
    Stop,
}

/// Events sent from the backend to the GUI.
#[derive(Debug, Clone)]
pub enum UiEvent {
    LocalPeerId(String),
    PeerDiscovered { peer_id: String, addr: String },
    PeerConnected(String),
    PeerDisconnected(String),
    SyncProgress { file_name: String, percent: f32 },
    FileComplete(String),
    SyncComplete,
    Log(String),
    Error(String),
}

/// Run the libp2p backend on a tokio runtime, communicating with the GUI via channels.
pub async fn run_backend(
    mut cmd_rx: mpsc::Receiver<UiCommand>,
    event_tx: mpsc::Sender<UiEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting Vitruvius backend...");

    let mut swarm = setup_network().await?;

    // Send local peer id to the GUI
    let local_peer_id = *swarm.local_peer_id();
    let _ = event_tx
        .send(UiEvent::LocalPeerId(local_peer_id.to_string()))
        .await;
    let _ = event_tx
        .send(UiEvent::Log("Backend started. Waiting for sync configuration...".into()))
        .await;

    // Wait for the StartSync command before entering the main event loop
    let (target_peer_id, sync_path_abs) = loop {
        match cmd_rx.recv().await {
            Some(UiCommand::StartSync {
                peer_id_str,
                folder_path,
            }) => {
                let target = if peer_id_str.trim().is_empty() {
                    None
                } else {
                    match peer_id_str.trim().parse::<PeerId>() {
                        Ok(id) => Some(id),
                        Err(e) => {
                            let _ = event_tx
                                .send(UiEvent::Error(format!("Invalid Peer ID: {}", e)))
                                .await;
                            continue;
                        }
                    }
                };
                let sync_path = PathBuf::from(&folder_path);
                if !sync_path.exists() {
                    if let Err(e) = fs::create_dir_all(&sync_path) {
                        let _ = event_tx
                            .send(UiEvent::Error(format!("Cannot create folder: {}", e)))
                            .await;
                        continue;
                    }
                }
                match fs::canonicalize(&sync_path) {
                    Ok(abs) => {
                        let _ = event_tx
                            .send(UiEvent::Log(format!("Syncing folder: {:?}", abs)))
                            .await;
                        break (target, abs);
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(UiEvent::Error(format!("Invalid folder path: {}", e)))
                            .await;
                        continue;
                    }
                }
            }
            Some(UiCommand::Stop) | None => {
                info!("Backend received stop before sync started.");
                return Ok(());
            }
        }
    };

    if let Some(target) = target_peer_id {
        let _ = event_tx
            .send(UiEvent::Log(format!("Will sync with peer: {}", target)))
            .await;
    } else {
        let _ = event_tx
            .send(UiEvent::Log("Waiting for incoming connections...".into()))
            .await;
    }

    let mut sync_state = SyncState::new();
    let mut currently_writing: HashSet<String> = HashSet::new();
    let mut recently_remote: HashSet<String> = HashSet::new();
    let mut cached_chunks: HashMap<String, (Vec<crate::storage::Chunk>, crate::storage::FileMetadata)> =
        HashMap::new();

    // Set up file watcher
    let (file_tx, mut file_rx) = mpsc::channel::<Event>(100);
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let _ = file_tx.blocking_send(event);
        }
    })?;
    watcher.watch(&sync_path_abs, RecursiveMode::Recursive)?;
    let _ = event_tx
        .send(UiEvent::Log("Watching folder for changes".into()))
        .await;

    loop {
        tokio::select! {
            // Check for GUI commands (non-blocking in select)
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(UiCommand::Stop) | None => {
                        info!("Backend shutting down...");
                        let _ = event_tx.send(UiEvent::Log("Shutting down...".into())).await;
                        break;
                    }
                    _ => {}
                }
            }

            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, addr) in list {
                            info!("Discovered peer: {} at {}", peer_id, addr);
                            let _ = event_tx
                                .send(UiEvent::PeerDiscovered {
                                    peer_id: peer_id.to_string(),
                                    addr: addr.to_string(),
                                })
                                .await;

                            if let Some(target) = target_peer_id {
                                if peer_id == target {
                                    let _ = event_tx
                                        .send(UiEvent::Log("Target peer found, dialing...".into()))
                                        .await;
                                    swarm.dial(addr)?;
                                }
                            }
                        }
                    }

                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        info!("Connected to {}", peer_id);
                        let _ = event_tx
                            .send(UiEvent::PeerConnected(peer_id.to_string()))
                            .await;

                        if !sync_state.connected_peers.contains(&peer_id) {
                            sync_state.connected_peers.push(peer_id);
                        }

                        if let Some(target) = target_peer_id {
                            if peer_id == target {
                                sync_state.sync_target_peer = Some(peer_id);
                                let _ = event_tx
                                    .send(UiEvent::Log("Requesting file list...".into()))
                                    .await;
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
                                        let _ = event_tx.send(UiEvent::Log("Peer requested file list".into())).await;
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

                                        if let SyncMessage::Metadata { ref file_name, .. } = response {
                                            if !cached_chunks.contains_key(file_name) {
                                                let file_path = sync_path_abs.join(file_name);
                                                if let Ok((chunks, mut metadata)) = crate::storage::chunk_file(&file_path) {
                                                    metadata.file_name = file_name.clone();
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
                                            let file_path = sync_path_abs.join(&file_name);
                                            match crate::storage::chunk_file(&file_path) {
                                                Ok((chunks, mut metadata)) => {
                                                    metadata.file_name = file_name.clone();
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
                                        let _ = event_tx.send(UiEvent::Log(format!("Peer notified file changed: {}", file_name))).await;
                                        cached_chunks.remove(&file_name);
                                        recently_remote.insert(file_name.clone());

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

                                    SyncMessage::DirectoryCreated { path } => {
                                        let _ = event_tx.send(UiEvent::Log(format!("Peer requested directory create: {}", path))).await;
                                        let dir_path = sync_path_abs.join(&path);
                                        if let Err(e) = fs::create_dir_all(&dir_path) {
                                            warn!("Failed to create directory {}: {:?}", path, e);
                                        } else {
                                            recently_remote.insert(path.clone());
                                        }

                                        let _ = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::TransferComplete,
                                        );
                                    }

                                    SyncMessage::DirectoryDeleted { path } => {
                                        let _ = event_tx.send(UiEvent::Log(format!("Peer requested directory delete: {}", path))).await;
                                        let dir_path = sync_path_abs.join(&path);
                                        if dir_path.exists() {
                                            if let Err(e) = fs::remove_dir_all(&dir_path) {
                                                warn!("Failed to remove directory {}: {:?}", path, e);
                                            } else {
                                                recently_remote.insert(path.clone());
                                            }
                                        }
                                        let _ = swarm.behaviour_mut().rr.send_response(
                                            channel,
                                            SyncMessage::TransferComplete,
                                        );
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
                                        let _ = event_tx.send(UiEvent::Log(format!("Received file list with {} files", file_names.len()))).await;

                                        if file_names.is_empty() {
                                            let _ = event_tx.send(UiEvent::Log("Remote folder is empty, sync complete".into())).await;
                                            let _ = event_tx.send(UiEvent::SyncComplete).await;
                                            sync_state.is_initial_sync_complete = true;
                                            continue;
                                        }

                                        let mut sync_session = SyncSession::new(file_names, MAX_CONCURRENT_FILES);

                                        while sync_session.can_start_more() {
                                            if let Some(file_name) = sync_session.start_next() {
                                                let _ = event_tx.send(UiEvent::Log(format!("Requesting metadata for: {}", file_name))).await;
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
                                        let _ = event_tx.send(UiEvent::Log(format!(
                                            "Received metadata for '{}' ({} chunks, {} bytes)",
                                            file_name, total_chunks, file_size
                                        ))).await;

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

                                        let _ = event_tx.send(UiEvent::SyncProgress {
                                            file_name: file_name.clone(),
                                            percent: 0.0,
                                        }).await;

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
                                                        // Send progress update
                                                        let _ = event_tx.send(UiEvent::SyncProgress {
                                                            file_name: file_name.clone(),
                                                            percent: transfer_needed.progress_percent(),
                                                        }).await;

                                                        if complete {
                                                            currently_writing.insert(file_name.clone());
                                                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                                                            currently_writing.remove(&file_name);

                                                            let _ = event_tx.send(UiEvent::FileComplete(file_name.clone())).await;

                                                            sync_state.transfer_states.remove(&file_name);
                                                            cached_chunks.remove(&file_name);

                                                            if let Some(session) = &mut sync_state.current_session {
                                                                session.mark_complete(&file_name);

                                                                while session.can_start_more() {
                                                                    if let Some(next_file) = session.start_next() {
                                                                        let _ = event_tx.send(UiEvent::Log(format!("Starting next file: {}", next_file))).await;
                                                                        if let Some(target) = sync_state.sync_target_peer {
                                                                            swarm.behaviour_mut().rr.send_request(
                                                                                &target,
                                                                                SyncMessage::MetaDataRequest { file_name: next_file },
                                                                            );
                                                                        }
                                                                    }
                                                                }

                                                                if session.is_done() {
                                                                    let _ = event_tx.send(UiEvent::Log("All files synchronized successfully".into())).await;
                                                                    let _ = event_tx.send(UiEvent::SyncComplete).await;
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
                                                        let _ = event_tx.send(UiEvent::Error(format!("Chunk {} error: {}", chunk_index, e))).await;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    SyncMessage::TransferComplete => {
                                        let _ = event_tx.send(UiEvent::Log("Peer acknowledged file change notification".into())).await;
                                    }

                                    SyncMessage::Empty => {
                                        let _ = event_tx.send(UiEvent::Log("Remote folder is empty".into())).await;
                                        let _ = event_tx.send(UiEvent::SyncComplete).await;
                                        sync_state.is_initial_sync_complete = true;
                                    }

                                    SyncMessage::Error { message } => {
                                        error!("Error from {}: {}", peer, message);
                                        let _ = event_tx.send(UiEvent::Error(format!("Error from {}: {}", peer, message))).await;
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
                        let _ = event_tx.send(UiEvent::Error(format!("Connection failed: {}", error))).await;
                    }

                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        info!("Connection closed with {}", peer_id);
                        let _ = event_tx.send(UiEvent::PeerDisconnected(peer_id.to_string())).await;
                        sync_state.connected_peers.retain(|p| p != &peer_id);
                        if sync_state.sync_target_peer == Some(peer_id) {
                            sync_state.sync_target_peer = None;
                            let _ = event_tx.send(UiEvent::Log("Sync target disconnected".into())).await;
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
                            let rel = match path.strip_prefix(&sync_path_abs) {
                                Ok(p) => p.to_string_lossy().to_string(),
                                Err(_) => path.to_string_lossy().to_string(),
                            };

                            if recently_remote.remove(&rel) {
                                continue;
                            }

                            if path.is_dir() {
                                let _ = event_tx.send(UiEvent::Log(format!("Directory created: {}", rel))).await;
                                for peer in &sync_state.connected_peers {
                                    swarm.behaviour_mut().rr.send_request(
                                        peer,
                                        SyncMessage::DirectoryCreated { path: rel.clone() },
                                    );
                                }
                                continue;
                            }

                            if path.is_file() {
                                let rel_path = rel.clone();

                                if currently_writing.contains(&rel_path) {
                                    continue;
                                }

                                if rel_path.starts_with('.') || rel_path.ends_with(".tmp") {
                                    continue;
                                }

                                let _ = event_tx.send(UiEvent::Log(format!("File changed: {}", rel_path))).await;

                                cached_chunks.remove(&rel_path);

                                for peer in &sync_state.connected_peers {
                                    swarm.behaviour_mut().rr.send_request(
                                        peer,
                                        SyncMessage::FileChanged {
                                            file_name: rel_path.clone(),
                                        }
                                    );
                                }
                            }
                        }
                    }
                    EventKind::Remove(remove_kind) => {
                        for path in file_event.paths {
                            let rel = match path.strip_prefix(&sync_path_abs) {
                                Ok(p) => p.to_string_lossy().to_string(),
                                Err(_) => path.to_string_lossy().to_string(),
                            };

                            if recently_remote.remove(&rel) {
                                continue;
                            }

                            match remove_kind {
                                notify::event::RemoveKind::Folder => {
                                    let _ = event_tx.send(UiEvent::Log(format!("Directory removed: {}", rel))).await;
                                    for peer in &sync_state.connected_peers {
                                        swarm.behaviour_mut().rr.send_request(
                                            peer,
                                            SyncMessage::DirectoryDeleted { path: rel.clone() },
                                        );
                                    }
                                }
                                _ => {
                                    let rel_path = rel.clone();
                                    let _ = event_tx.send(UiEvent::Log(format!("File removed: {}", rel_path))).await;

                                    cached_chunks.remove(&rel_path);

                                    for peer in &sync_state.connected_peers {
                                        swarm.behaviour_mut().rr.send_request(
                                            peer,
                                            SyncMessage::FileDeleted {
                                                file_name: rel_path.clone(),
                                            }
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down...");
                let _ = event_tx.send(UiEvent::Log("Shutting down...".into())).await;
                break;
            }
        }
    }

    Ok(())
}
