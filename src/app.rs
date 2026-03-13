use crate::network::{MyBehaviourEvent, SyncMessage, SyncSession, setup_network};
use crate::storage::{FileTransferState, FileVersion, get_file_list, get_versioned_metadata, process_received_chunk};
use crate::sync::SyncState;
use anyhow::Result;
use dialoguer::Input;
use libp2p::{PeerId, futures::StreamExt, mdns, request_response, swarm::SwarmEvent};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const MAX_CONCURRENT_FILES: usize = 3;
const MAX_CONCURRENT_CHUNKS: usize = 8;

pub async fn run() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting Vitruvius with Delta-Aware Sync...");

    let mut swarm = setup_network().await?;

    let mut sync_state = SyncState::new();
    let mut currently_writing: HashSet<String> = HashSet::new();
    // paths that were modified/created/deleted as a result of incoming network activity
    // we ignore watcher events for these to avoid loops
    let mut recently_remote: HashSet<String> = HashSet::new();

    // Cache for chunked files on the serving side
    let mut cached_chunks: HashMap<String, (Vec<crate::storage::Chunk>, crate::storage::FileMetadata)> =
        HashMap::new();
    
    // Track file versions for delta detection 
    let mut local_file_versions: HashMap<String, FileVersion> = HashMap::new();
    let mut remote_file_versions: HashMap<String, FileVersion> = HashMap::new();

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

    let local_peer_id = swarm.local_peer_id().clone();
    let peer_id_str = local_peer_id.to_string();

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

                                        // Use versioned metadata for delta detection
                                        let response = match get_versioned_metadata(&sync_path_abs, &file_name, peer_id_str.clone(), &local_file_versions).await {
                                            Ok(msg) => msg,
                                            Err(e) => {
                                                SyncMessage::Error {
                                                    message: format!("Failed to get metadata: {}", e),
                                                }
                                            }
                                        };

                                        // Cache the chunks if metadata was successful
                                        if let SyncMessage::VersionedMetadata { ref file_name, .. } = response {
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
                                            // Fallback: load and chunk the file
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

                                    SyncMessage::FileChangedWithVersion { file_name, file_hash, changed_chunk_count, .. } => {
                                        info!("Peer notified file changed (version): {} - {} chunks changed", file_name, changed_chunk_count);

                                        // Check if we already have this version
                                        if let Some(prev_version) = remote_file_versions.get(&file_name) {
                                            if prev_version.file_hash == file_hash {
                                                info!("Already have latest version for {}, skipping", file_name);
                                                if let Err(e) = swarm.behaviour_mut().rr.send_response(
                                                    channel,
                                                    SyncMessage::TransferComplete,
                                                ) {
                                                    warn!("Failed to send response: {:?}", e);
                                                }
                                                continue;
                                            }
                                        }

                                        // Clear cache and mark as remote
                                        cached_chunks.remove(&file_name);
                                        recently_remote.insert(file_name.clone());

                                        // Request full metadata with delta details
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

                                    SyncMessage::FileChanged { file_name } => {
                                        info!("Peer notified file changed: {}", file_name);

                                        // Clear cache for this file
                                        cached_chunks.remove(&file_name);

                                        // mark as remote so watcher skips it
                                        recently_remote.insert(file_name.clone());

                                        // Request metadata to check what changed (including delta info)
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
                                        info!("Peer requested directory create: {}", path);
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
                                        info!("Peer requested directory delete: {}", path);
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

                                    SyncMessage::FileDeleted { file_name } => {
                                        info!("Peer requested file delete: {}", file_name);
                                        let file_path = sync_path_abs.join(&file_name);
                                        if file_path.exists() {
                                            if let Err(e) = fs::remove_file(&file_path) {
                                                warn!("Failed to remove file {}: {:?}", file_name, e);
                                            } else {
                                                recently_remote.insert(file_name.clone());
                                            }
                                        }

                                        // Clear from cache
                                        cached_chunks.remove(&file_name);

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

                                    SyncMessage::VersionedMetadata { file_name, file_hash, parent_hash: _, total_chunks, file_size, chunk_hashes, changed_chunks, timestamp: _, author } => {
                                        info!("Received versioned metadata for '{}' ({} chunks, {} bytes, {} changed)",
                                              file_name, total_chunks, file_size, changed_chunks.len());

                                        let metadata = crate::storage::FileMetadata {
                                            file_name: file_name.clone(),
                                            total_chunks,
                                            file_size,
                                            chunk_hashes: chunk_hashes.clone(),
                                        };

                                        let version = crate::storage::FileVersion::from_parent(
                                            file_hash,
                                            metadata.clone(),
                                            author.clone(),
                                            remote_file_versions.get(&file_name).unwrap_or(&crate::storage::FileVersion::new(
                                                [0u8; 32],
                                                crate::storage::FileMetadata {
                                                    file_name: file_name.clone(),
                                                    total_chunks: 0,
                                                    file_size: 0,
                                                    chunk_hashes: vec![],
                                                },
                                                "unknown".to_string(),
                                            )),
                                            changed_chunks.clone(),
                                        );

                                        remote_file_versions.insert(file_name.clone(), version.clone());

                                        let mut transfer_state = FileTransferState::new(sync_path_abs.clone());
                                        transfer_state.metadata = Some(metadata.clone());
                                        transfer_state.current_version = Some(version.clone());
                                        transfer_state.set_required_chunks();
                                        
                                        sync_state.transfer_states.insert(file_name.clone(), transfer_state);

                                        // Request only changed chunks (delta aware)
                                        let chunks_to_request = std::cmp::min(MAX_CONCURRENT_CHUNKS, changed_chunks.len());
                                        for i in 0..chunks_to_request {
                                            if let Some(chunk_idx) = changed_chunks.get(i) {
                                                if let Some(target) = sync_state.sync_target_peer {
                                                    swarm.behaviour_mut().rr.send_request(
                                                        &target,
                                                        SyncMessage::ChunkRequest {
                                                            file_name: file_name.clone(),
                                                            chunk_index: *chunk_idx,
                                                        }
                                                    );
                                                }
                                            }
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
                                        transfer_state.metadata = Some(metadata.clone());
                                        transfer_state.chunks_expected = total_chunks;
                                        transfer_state.set_required_chunks();
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

                                        if transfer_needed.metadata.is_some() {
                                            if !transfer_needed.required_chunks.is_empty() && !transfer_needed.received_chunks.contains_key(&chunk_index) {
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

                                                            // Update local version tracking
                                                            if let Some(version) = transfer_needed.current_version.clone() {
                                                                local_file_versions.insert(file_name.clone(), version);
                                                            }

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
                                                            // More chunks needed
                                                            let required_chunks: Vec<usize> = transfer_needed.required_chunks.iter().cloned().collect();
                                                            let received_count = transfer_needed.received_chunks.iter()
                                                                .filter(|(idx, _)| transfer_needed.required_chunks.contains(idx))
                                                                .count();
                                                            
                                                            // Request next chunk if available slots
                                                            if received_count < required_chunks.len() && transfer_needed.chunks_in_flight < MAX_CONCURRENT_CHUNKS {
                                                                // Find next unrequested required chunk
                                                                for &req_chunk in &required_chunks {
                                                                    if !transfer_needed.received_chunks.contains_key(&req_chunk) {
                                                                        if let Some(target) = sync_state.sync_target_peer {
                                                                            transfer_needed.chunks_in_flight += 1;
                                                                            swarm.behaviour_mut().rr.send_request(
                                                                                &target,
                                                                                SyncMessage::ChunkRequest {
                                                                                    file_name: file_name.clone(),
                                                                                    chunk_index: req_chunk,
                                                                                }
                                                                            );
                                                                        }
                                                                        break;
                                                                    }
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
                            // compute relative path string for messaging and skip checks
                            let rel = match path.strip_prefix(&sync_path_abs) {
                                Ok(p) => p.to_string_lossy().to_string(),
                                Err(_) => path.to_string_lossy().to_string(),
                            };

                            if recently_remote.remove(&rel) {
                                // change originated from remote; ignore
                                continue;
                            }

                            if path.is_dir() {
                                info!("Directory created: {}", rel);
                                for peer in &sync_state.connected_peers {
                                    swarm.behaviour_mut().rr.send_request(
                                        peer,
                                        SyncMessage::DirectoryCreated { path: rel.clone() },
                                    );
                                }
                                continue;
                            }

                            if path.is_file() {
                                // send the path relative to the sync root so nested files work
                                let rel_path = rel.clone();

                                // Skip files we're currently writing
                                if currently_writing.contains(&rel_path) {
                                    continue;
                                }

                                // Skip temporary files
                                if rel_path.starts_with('.') || rel_path.ends_with(".tmp") {
                                    continue;
                                }

                                info!("File changed: {}", rel_path);

                                // Invalidate cache for this file
                                cached_chunks.remove(&rel_path);

                                // Compute version info for efficient delta sync
                                if let Ok((chunks, metadata)) = crate::storage::chunk_file(&path) {
                                    let version = if let Some(prev_version) = local_file_versions.get(&rel_path) {
                                        crate::storage::create_file_version_from_parent(&chunks, metadata, peer_id_str.clone(), prev_version)
                                    } else {
                                        crate::storage::create_file_version(&chunks, metadata, peer_id_str.clone())
                                    };

                                    local_file_versions.insert(rel_path.clone(), version.clone());

                                    // Notify all peers with version info
                                    for peer_ref in &sync_state.connected_peers {
                                        swarm.behaviour_mut().rr.send_request(
                                            peer_ref,
                                            SyncMessage::FileChangedWithVersion {
                                                file_name: rel_path.clone(),
                                                file_hash: version.file_hash,
                                                changed_chunk_count: version.changed_chunks.len(),
                                                timestamp: version.timestamp,
                                            }
                                        );
                                    }
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
                                    info!("Directory removed: {}", rel);
                                    for peer in &sync_state.connected_peers {
                                        swarm.behaviour_mut().rr.send_request(
                                            peer,
                                            SyncMessage::DirectoryDeleted { path: rel.clone() },
                                        );
                                    }
                                }
                                _ => {
                                    // treat every non-folder removal as file removal; rel is already relative
                                    let rel_path = rel.clone();
                                    info!("File removed: {}", rel_path);

                                    // Remove from cache
                                    cached_chunks.remove(&rel_path);

                                    // Notify peers about deletion
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
                break;
            }
        }
    }

    Ok(())
}
