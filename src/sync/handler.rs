// src/sync/handler.rs
//
// ── Download scheduler design ─────────────────────────────────────────────────
//
// THE BUG WE ARE FIXING:
//   When a manifest with N files arrives, the old code fired N × WINDOW chunk
//   requests simultaneously. With 84 files × 8 = 672 concurrent requests, the
//   TCP stream buffer overflows → "Broken pipe" → disconnection → reconnect →
//   same blast again → infinite crash loop.
//
// THE FIX — a two-level queue:
//
//   pending_files: VecDeque<PendingFile>
//     Files from the manifest that we know we need but haven't started yet.
//     Populated when a manifest arrives. Never touches the network.
//
//   active_transfers: HashMap<file_name, FileTransferState>
//     Files currently being downloaded (chunk requests in-flight).
//     Capped at MAX_CONCURRENT_FILES at all times.
//
//   When a file completes → remove from active → pop from pending → start next.
//   This keeps concurrent streams at MAX_CONCURRENT_FILES × WINDOW = 3 × 4 = 12.
//   Well within libp2p's 64-stream limit even for thousands of files.
//
// ── Sync trigger design ───────────────────────────────────────────────────────
//   1. SetFolder (while connected) → FolderAnnouncement + ManifestRequest to all peers
//   2. ConnectionEstablished (we have folder) → same for that peer
//   3. Receiving FolderAnnouncement → ManifestRequest; reply with our announcement ONCE
//
// ── Empty vs Ack ─────────────────────────────────────────────────────────────
//   SyncMessage::Empty — "I have no folder or no files" (response to ManifestRequest only)
//   SyncMessage::Ack   — pure protocol close (response to FolderAnnouncement / TransferComplete)

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use libp2p::{mdns, request_response, swarm::SwarmEvent, Multiaddr, PeerId, Swarm};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::gui::{GuiCommand, GuiEvent, GuiFileInfo};
use crate::network::{MyBehaviour, MyBehaviourEvent, SyncMessage};
use crate::state::{short_id, AppState};
use crate::storage::{self, FileMetadata, FileTransferState, PendingFile};

/// Chunks in-flight per active file.
/// 4 is a safe default: 3 files × 4 = 12 concurrent streams, well within limits.
const WINDOW: usize = 4;

/// Maximum files downloading at the same time.
/// Controls total stream count = MAX_CONCURRENT_FILES × WINDOW.
const MAX_CONCURRENT_FILES: usize = 3;

/// Seconds without a chunk before a file is considered stalled.
const STALL_SECS: u64 = 20;

// ─── Per-peer download state ──────────────────────────────────────────────────
// Stored in the transfers map as the value.
// active: files currently downloading (chunk requests in-flight)
// queue:  files waiting to start (no network traffic yet)
pub struct PeerDownload {
    pub active: HashMap<String, FileTransferState>,
    pub queue: VecDeque<PendingFile>,
}

impl PeerDownload {
    fn new() -> Self {
        Self {
            active: HashMap::new(),
            queue: VecDeque::new(),
        }
    }
}

// =============================================================================
// GUI COMMAND HANDLER
// =============================================================================

pub async fn on_command(
    cmd: GuiCommand,
    swarm: &mut Swarm<MyBehaviour>,
    state: Arc<Mutex<AppState>>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
    watch_tx: &mpsc::UnboundedSender<PathBuf>,
    node_name: &str,
) {
    match cmd {
        GuiCommand::SetFolder { path } => {
            let p = PathBuf::from(&path);
            if !p.exists() {
                if let Err(e) = fs::create_dir_all(&p) {
                    return log(event_tx, "ERROR", format!("Cannot create folder: {e}"));
                }
            }
            let abs = match fs::canonicalize(&p) {
                Ok(a) => a,
                Err(e) => return log(event_tx, "ERROR", format!("Bad path: {e}")),
            };
            let _ = watch_tx.send(abs.clone());

            match storage::list_folder(&abs).await {
                Ok(files) => {
                    let listing: Vec<GuiFileInfo> = files
                        .iter()
                        .map(|f| GuiFileInfo {
                            name: f.file_name.clone(),
                            size: f.file_size,
                            chunks: f.total_chunks,
                        })
                        .collect();
                    let count = listing.len();
                    let _ = event_tx.send(GuiEvent::FolderListing { files: listing });
                    log(
                        event_tx,
                        "OK",
                        format!("Sync folder set: {} ({count} file(s))", abs.display()),
                    );
                }
                Err(e) => log(
                    event_tx,
                    "WARN",
                    format!("Folder set but listing failed: {e}"),
                ),
            }

            let connected: Vec<PeerId> = {
                let mut st = state.lock().await;
                st.sync_path = Some(abs);
                st.announced_to.clear();
                st.connected_peers.iter().cloned().collect()
            };

            for peer in connected {
                let pid_str = peer.to_string();
                state.lock().await.announced_to.insert(peer);
                swarm.behaviour_mut().rr.send_request(
                    &peer,
                    SyncMessage::FolderAnnouncement {
                        node_name: node_name.to_string(),
                    },
                );
                swarm
                    .behaviour_mut()
                    .rr
                    .send_request(&peer, SyncMessage::ManifestRequest);
                log(
                    event_tx,
                    "INFO",
                    format!(
                        "Announced folder to {} and requested their manifest",
                        short_id(&pid_str)
                    ),
                );
            }
        }

        GuiCommand::DialPeer { peer_id, addr } => {
            let addr_str = addr.or_else(|| {
                state
                    .try_lock()
                    .ok()
                    .and_then(|s| s.known_addrs.get(&peer_id).cloned())
            });
            match addr_str {
                None => log(
                    event_tx,
                    "WARN",
                    format!("No address known for {}", short_id(&peer_id)),
                ),
                Some(a) => match a.parse::<Multiaddr>() {
                    Err(e) => log(event_tx, "ERROR", format!("Invalid multiaddr: {e}")),
                    Ok(ma) => {
                        log(
                            event_tx,
                            "INFO",
                            format!("Dialing {} …", short_id(&peer_id)),
                        );
                        if let Err(e) = swarm.dial(ma) {
                            log(event_tx, "ERROR", format!("Dial error: {e}"));
                        }
                    }
                },
            }
        }

        GuiCommand::RequestSync { peer_id } => {
            if let Ok(pid) = peer_id.parse::<PeerId>() {
                log(
                    event_tx,
                    "INFO",
                    format!("Requesting manifest from {} …", short_id(&peer_id)),
                );
                swarm
                    .behaviour_mut()
                    .rr
                    .send_request(&pid, SyncMessage::ManifestRequest);
            }
        }

        GuiCommand::Disconnect { peer_id } => {
            if let Ok(pid) = peer_id.parse::<PeerId>() {
                let _ = swarm.disconnect_peer_id(pid);
            }
        }
    }
}

// =============================================================================
// STALL CHECKER — fires every 10 seconds from main.rs
// =============================================================================

pub async fn check_stalls(
    swarm: &mut Swarm<MyBehaviour>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
    transfers: &mut HashMap<PeerId, PeerDownload>,
) {
    let timeout = Duration::from_secs(STALL_SECS);

    for (peer, dl) in transfers.iter_mut() {
        for (file_name, ts) in dl.active.iter_mut() {
            if ts.last_activity.elapsed() < timeout {
                continue;
            }
            if ts.metadata.is_none() {
                continue;
            }

            let missing = ts.missing_chunks();
            if missing.is_empty() {
                continue;
            }

            let total = ts.metadata.as_ref().map(|m| m.total_chunks).unwrap_or(0);
            warn!(
                "Stall: {} ({}/{}) — re-requesting {} chunks",
                file_name,
                ts.received_chunks.len(),
                total,
                missing.len()
            );
            log(
                event_tx,
                "WARN",
                format!(
                    "Stall on {file_name} — re-requesting {} chunks",
                    missing.len()
                ),
            );

            // Re-request up to WINDOW missing chunks
            let to_req = missing.len().min(WINDOW);
            for &ci in missing.iter().take(to_req) {
                swarm.behaviour_mut().rr.send_request(
                    peer,
                    SyncMessage::ChunkRequest {
                        file_name: file_name.clone(),
                        chunk_index: ci,
                    },
                );
            }
            // Advance next_request past what we just re-requested
            if let Some(&last_ci) = missing.iter().take(to_req).last() {
                ts.next_request = ts.next_request.max(last_ci + 1);
            }
            ts.last_activity = std::time::Instant::now();
        }
    }
}

// =============================================================================
// SWARM EVENT HANDLER
// =============================================================================

pub async fn on_swarm_event(
    event: SwarmEvent<MyBehaviourEvent>,
    swarm: &mut Swarm<MyBehaviour>,
    state: Arc<Mutex<AppState>>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
    transfers: &mut HashMap<PeerId, PeerDownload>,
) {
    match event {
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            for (peer_id, addr) in list {
                let pid_str = peer_id.to_string();
                let addr_str = addr.to_string();
                state
                    .lock()
                    .await
                    .known_addrs
                    .insert(pid_str.clone(), addr_str.clone());
                let display = peer_display_name(&state, &pid_str).await;
                info!("mDNS: {} at {}", pid_str, addr_str);
                let _ = event_tx.send(GuiEvent::PeerDiscovered {
                    peer_id: pid_str,
                    addr: addr_str,
                    node_name: display,
                });
            }
        }

        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, _) in list {
                state.lock().await.known_addrs.remove(&peer_id.to_string());
            }
        }

        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            let pid_str = peer_id.to_string();
            state.lock().await.connected_peers.insert(peer_id);
            info!("Connected to {}", pid_str);

            let display = peer_display_name(&state, &pid_str).await;
            let _ = event_tx.send(GuiEvent::PeerConnected {
                peer_id: pid_str.clone(),
                node_name: display.clone(),
            });
            log(event_tx, "OK", format!("Connected to {display}"));

            let (have_folder, my_name) = folder_status(&state).await;
            if have_folder {
                state.lock().await.announced_to.insert(peer_id);
                swarm.behaviour_mut().rr.send_request(
                    &peer_id,
                    SyncMessage::FolderAnnouncement { node_name: my_name },
                );
                swarm
                    .behaviour_mut()
                    .rr
                    .send_request(&peer_id, SyncMessage::ManifestRequest);
                log(
                    event_tx,
                    "INFO",
                    format!(
                        "Folder already set — announced to {} and requesting their manifest",
                        short_id(&pid_str)
                    ),
                );
            }
        }

        SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
            let pid_str = peer_id.to_string();
            {
                let mut st = state.lock().await;
                st.connected_peers.remove(&peer_id);
                st.announced_to.remove(&peer_id);
            }
            // Drop all download state — will restart cleanly on reconnect
            transfers.remove(&peer_id);
            let reason = cause.map(|e| e.to_string()).unwrap_or_default();
            warn!("Closed: {} — {}", pid_str, reason);
            let _ = event_tx.send(GuiEvent::PeerDisconnected {
                peer_id: pid_str.clone(),
            });
            log(
                event_tx,
                "WARN",
                format!("Disconnected from {} ({reason})", short_id(&pid_str)),
            );
        }

        SwarmEvent::Behaviour(MyBehaviourEvent::Rr(request_response::Event::Message {
            peer,
            message,
        })) => {
            let pid_str = peer.to_string();
            match message {
                request_response::Message::Request {
                    channel, request, ..
                } => on_request(request, channel, peer, &pid_str, swarm, &state, event_tx).await,
                request_response::Message::Response { response, .. } => {
                    on_response(response, peer, &pid_str, swarm, &state, event_tx, transfers).await
                }
            }
        }

        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            let pid_str = peer_id.map(|p| p.to_string());
            warn!("Dial failed {:?}: {}", pid_str, error);
            let _ = event_tx.send(GuiEvent::DialFailed {
                peer_id: pid_str,
                error: error.to_string(),
            });
        }

        _ => {}
    }
}

// =============================================================================
// UPLOADER — serve an incoming request
// =============================================================================

async fn on_request(
    request: SyncMessage,
    channel: request_response::ResponseChannel<SyncMessage>,
    peer: PeerId,
    pid_str: &str,
    swarm: &mut Swarm<MyBehaviour>,
    state: &Arc<Mutex<AppState>>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
) {
    match request {
        SyncMessage::FolderAnnouncement {
            node_name: peer_name,
        } => {
            state
                .lock()
                .await
                .peer_names
                .insert(pid_str.to_string(), peer_name.clone());
            log(
                event_tx,
                "INFO",
                format!("{peer_name} announced their folder — requesting their manifest …"),
            );
            // ACK closes the RR channel without semantic meaning
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SyncMessage::Ack);
            // Always request their manifest
            swarm
                .behaviour_mut()
                .rr
                .send_request(&peer, SyncMessage::ManifestRequest);
            // Reply with our announcement ONLY if we haven't already — prevents ping-pong loop
            let (have_folder, my_name) = folder_status(state).await;
            if have_folder {
                let already = state.lock().await.announced_to.contains(&peer);
                if !already {
                    state.lock().await.announced_to.insert(peer);
                    swarm.behaviour_mut().rr.send_request(
                        &peer,
                        SyncMessage::FolderAnnouncement { node_name: my_name },
                    );
                }
            }
        }

        SyncMessage::ManifestRequest => {
            let (path, my_name) = {
                let st = state.lock().await;
                (st.sync_path.clone(), st.node_name.clone())
            };
            match path {
                None => {
                    log(
                        event_tx,
                        "INFO",
                        format!(
                            "{} requested manifest — no folder set yet",
                            short_id(pid_str)
                        ),
                    );
                    let _ = swarm
                        .behaviour_mut()
                        .rr
                        .send_response(channel, SyncMessage::Empty);
                }
                Some(ref p) => {
                    log(
                        event_tx,
                        "INFO",
                        format!("{} requested our manifest", short_id(pid_str)),
                    );
                    let resp: SyncMessage = storage::get_manifest(p, &my_name)
                        .await
                        .unwrap_or_else(|e: anyhow::Error| SyncMessage::Error {
                            message: e.to_string(),
                        });
                    let _ = swarm.behaviour_mut().rr.send_response(channel, resp);
                }
            }
        }

        SyncMessage::ChunkRequest {
            ref file_name,
            chunk_index,
        } => {
            let path = state.lock().await.sync_path.clone();
            match path {
                None => {
                    let _ = swarm.behaviour_mut().rr.send_response(
                        channel,
                        SyncMessage::Error {
                            message: "No sync folder".into(),
                        },
                    );
                }
                Some(ref p) => {
                    let resp: SyncMessage = storage::get_chunk(p, file_name, chunk_index)
                        .await
                        .unwrap_or_else(|e: anyhow::Error| SyncMessage::Error {
                            message: e.to_string(),
                        });
                    if matches!(resp, SyncMessage::ChunkResponse { .. }) {
                        log(
                            event_tx,
                            "INFO",
                            format!("-> {file_name} [{chunk_index}] to {}", short_id(pid_str)),
                        );
                    }
                    let _ = swarm.behaviour_mut().rr.send_response(channel, resp);
                }
            }
        }

        SyncMessage::TransferComplete { ref file_name } => {
            log(
                event_tx,
                "OK",
                format!("{} confirmed receipt of {file_name}", short_id(pid_str)),
            );
            // ACK — not Empty — so the receiver doesn't think our folder is gone
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SyncMessage::Ack);
        }
        SyncMessage::FileChanged { ref file_name } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SyncMessage::Ack);

            let sync_path = state.lock().await.sync_path.clone();
            if let Some(ref path) = sync_path {
                let mut full_path = path.clone();
                for component in file_name.split('/') {
                    full_path.push(component);
                }
                // delete local copy so manifest check doesn't skip it
                state.lock().await.deleting_files.insert(full_path.clone());
let _ = std::fs::remove_file(&full_path);
let state2 = Arc::clone(state);  // need to capture for the spawn
let value = full_path.clone();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(2000)).await;
    state2.lock().await.deleting_files.remove(&value);
});
                let _ = std::fs::remove_file(&full_path);

                log(
                    event_tx,
                    "INFO",
                    format!(
                        "{} changed on {} — re-requesting",
                        file_name,
                        short_id(pid_str)
                    ),
                );

                swarm
                    .behaviour_mut()
                    .rr
                    .send_request(&peer, SyncMessage::ManifestRequest);
            }
        }

        SyncMessage::FileDeleted { ref file_name } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SyncMessage::Ack);

            let sync_path = state.lock().await.sync_path.clone();
            if let Some(ref path) = sync_path {
                let mut full_path = path.clone();
                for component in file_name.split('/') {
                    full_path.push(component);
                }
                if let Err(e) = std::fs::remove_file(&full_path) {
                    log(
                        event_tx,
                        "WARN",
                        format!("Could not delete {file_name}: {e}"),
                    );
                } else {
                    log(
                        event_tx,
                        "OK",
                        format!("{file_name} deleted (peer removed it)"),
                    );
                }
            }
        }

        _ => {
            let _ = swarm.behaviour_mut().rr.send_response(
                channel,
                SyncMessage::Error {
                    message: "Unexpected request type".into(),
                },
            );
        }
    }
}

// =============================================================================
// DOWNLOADER — handle a response to one of our requests
// =============================================================================
// =============================================================================
// SCHEDULER — promote files from queue → active up to MAX_CONCURRENT_FILES
// =============================================================================
async fn on_response(
    response: SyncMessage,
    peer: PeerId,
    pid_str: &str,
    swarm: &mut Swarm<MyBehaviour>,
    state: &Arc<Mutex<AppState>>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
    transfers: &mut HashMap<PeerId, PeerDownload>,
) {
    match response {
        SyncMessage::Ack => {}

        SyncMessage::Manifest {
            node_name: peer_name,
            ref files,
        } => {
            state
                .lock()
                .await
                .peer_names
                .insert(pid_str.to_string(), peer_name.clone());

            if files.is_empty() {
                log(event_tx, "INFO", format!("{peer_name} manifest returned no files"));
                return;
            }

            let sync_path = match state.lock().await.sync_path.clone() {
                Some(p) => p,
                None => {
                    log(
                        event_tx,
                        "WARN",
                        "Received manifest but no sync folder set — set one first!".into(),
                    );
                    return;
                }
            };

            let dl = transfers.entry(peer).or_insert_with(PeerDownload::new);

            let mut newly_queued = 0usize;
            for fe in files {
                if rel_path_exists(&sync_path, &fe.file_name) {
                    continue;
                }
                if dl.active.contains_key(&fe.file_name) {
                    continue;
                }
                if dl.queue.iter().any(|p| p.file_name == fe.file_name) {
                    continue;
                }

                dl.queue.push_back(PendingFile::from(fe));
                newly_queued += 1;
            }

            let total_pending = dl.active.len() + dl.queue.len();
            if total_pending == 0 {
                log(
                    event_tx,
                    "OK",
                    format!("All files from {peer_name} already synced — nothing to do"),
                );
                return;
            }

            log(
                event_tx,
                "INFO",
                format!(
                    "{peer_name}: {newly_queued} file(s) queued ({} active, {} waiting)",
                    dl.active.len(),
                    dl.queue.len()
                ),
            );

            start_queued_files(peer, &sync_path, dl, swarm, event_tx, pid_str);
        }

        SyncMessage::Empty => {
            let _ = event_tx.send(GuiEvent::RemoteEmpty {
                peer_id: pid_str.to_string(),
            });
            log(
                event_tx,
                "INFO",
                format!("{} has no sync folder set yet", short_id(pid_str)),
            );
        }

        SyncMessage::ChunkResponse {
            ref file_name,
            chunk_index,
            ref data,
            hash: _,
        } => {
            let sync_path = match state.lock().await.sync_path.clone() {
                Some(p) => p,
                None => return,
            };

            let dl = match transfers.get_mut(&peer) {
                Some(d) => d,
                None => return,
            };

            let ts = match dl.active.get_mut(file_name) {
                Some(t) => t,
                None => return,
            };

            let expected: Option<[u8; 32]> = ts
                .metadata
                .as_ref()
                .and_then(|m| m.chunk_hashes.get(chunk_index))
                .copied();

            let verified = expected
                .map(|h| storage::verify_chunk(data, &h))
                .unwrap_or(false);

            let total = ts.metadata.as_ref().map(|m| m.total_chunks).unwrap_or(0);

            let _ = event_tx.send(GuiEvent::ChunkReceived {
                peer_id: pid_str.to_string(),
                file_name: file_name.clone(),
                chunk_index,
                total_chunks: total,
                verified,
            });

            if !verified {
                log(
                    event_tx,
                    "ERROR",
                    format!("{file_name} chunk {chunk_index} hash mismatch — retrying"),
                );
                swarm.behaviour_mut().rr.send_request(
                    &peer,
                    SyncMessage::ChunkRequest {
                        file_name: file_name.clone(),
                        chunk_index,
                    },
                );
                return;
            }

            ts.received_chunks.insert(chunk_index, data.clone());
            ts.last_activity = std::time::Instant::now();

            if ts.next_request < total {
                swarm.behaviour_mut().rr.send_request(
                    &peer,
                    SyncMessage::ChunkRequest {
                        file_name: file_name.clone(),
                        chunk_index: ts.next_request,
                    },
                );
                ts.next_request += 1;
            }

            log(
                event_tx,
                "INFO",
                format!("{file_name}  {}/{total}", chunk_index + 1),
            );

            if ts.received_chunks.len() < total {
                return;
            }

            match storage::reassemble(ts).await {
                Ok(written_path) => {
                    state.lock().await.writing_files.insert(written_path.clone());

                    let fname = file_name.clone();

                    let _ = event_tx.send(GuiEvent::TransferComplete {
                        peer_id: pid_str.to_string(),
                        file_name: fname.clone(),
                    });

                    log(event_tx, "OK", format!("  {fname} saved to disk"));

                    swarm.behaviour_mut().rr.send_request(
                        &peer,
                        SyncMessage::TransferComplete {
                            file_name: fname.clone(),
                        },
                    );

                    dl.active.remove(&fname);
                    start_queued_files(peer, &sync_path, dl, swarm, event_tx, pid_str);

                    let state2 = Arc::clone(state);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        state2.lock().await.writing_files.remove(&written_path);
                    });

                    if !dl.queue.is_empty() || !dl.active.is_empty() {
                        log(
                            event_tx,
                            "INFO",
                            format!(
                                "{} active download(s), {} in queue",
                                dl.active.len(),
                                dl.queue.len()
                            ),
                        );
                    } else {
                        log(
                            event_tx,
                            "OK",
                            format!(
                                "All downloads from {} complete!",
                                short_id(pid_str)
                            ),
                        );
                    }
                }

                Err(e) => {
                    error!("Reassembly error for {file_name}: {e}");
                    log(event_tx, "ERROR", format!("Failed to write {file_name}: {e}"));
                }
            }
        }

        SyncMessage::Error { message } => {
            let _ = event_tx.send(GuiEvent::PeerError {
                peer_id: pid_str.to_string(),
                message: message.clone(),
            });
            log(
                event_tx,
                "ERROR",
                format!("Error from {}: {message}", short_id(pid_str)),
            );
        }

        _ => {
            warn!("Unexpected response from {}", pid_str);
        }
    }
}
fn start_queued_files(
    peer: PeerId,
    sync_path: &PathBuf,
    dl: &mut PeerDownload,
    swarm: &mut Swarm<MyBehaviour>,
    event_tx: &mpsc::UnboundedSender<GuiEvent>,
    pid_str: &str,
) {
    while dl.active.len() < MAX_CONCURRENT_FILES {
        let pf = match dl.queue.pop_front() {
            Some(f) => f,
            None => break,
        };

        if rel_path_exists(sync_path, &pf.file_name) {
            log(event_tx, "INFO", format!("{} already on disk — skipping", pf.file_name));
            continue;
        }

        // ── Stage 2: check local chunk index ─────────────────────────────
        // Build index synchronously-ish by blocking — this is cheap (just
        // hashing already-computed metadata). We use a blocking call here
        // because start_queued_files is a sync fn called from async context.
        let index = {
            let sp = sync_path.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(storage::build_chunk_index(&sp))
            })
        };

        let mut ts = FileTransferState::new(sync_path.clone());
        ts.metadata = Some(FileMetadata {
            file_name:    pf.file_name.clone(),
            total_chunks: pf.total_chunks,
            file_size:    pf.file_size,
            chunk_hashes: pf.chunk_hashes.clone(),
        });

        // ── Fill chunks we already have locally ───────────────────────────
        let mut local_hits = 0usize;
        for (chunk_index, hash) in pf.chunk_hashes.iter().enumerate() {
            if let Some((src_file, src_chunk)) = index.get(hash) {
                // read the data from the local file
                let sp = sync_path.clone();
                let src_file = src_file.clone();
                let src_chunk = *src_chunk;
                let data = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage::read_local_chunk(&sp, &src_file, src_chunk))
                });
                if let Some(bytes) = data {
                    ts.received_chunks.insert(chunk_index, bytes);
                    local_hits += 1;
                }
            }
        }

        let total = pf.total_chunks;
        let needed: Vec<usize> = (0..total)
            .filter(|i| !ts.received_chunks.contains_key(i))
            .collect();

        if local_hits > 0 {
            log(
                event_tx,
                "INFO",
                format!(
                    "{} — {}/{} chunks from local cache, {} to download",
                    pf.file_name, local_hits, total, needed.len()
                ),
            );
        }

        // ── Case A: file fully satisfied from local cache ─────────────────
        if needed.is_empty() {
            log(event_tx, "OK", format!("{} — fully deduped, 0 bytes from network", pf.file_name));

            let _ = event_tx.send(GuiEvent::TransferStarted {
                peer_id:      pid_str.to_string(),
                file_name:    pf.file_name.clone(),
                total_chunks: pf.total_chunks,
                file_size:    pf.file_size,
            });

            // reassemble immediately — no network needed
            let sp = sync_path.clone();
            let fname = pf.file_name.clone();
            let etx = event_tx.clone();
            let pid = pid_str.to_string();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match storage::reassemble(&ts).await {
                        Ok(_) => {
                            let _ = etx.send(GuiEvent::TransferComplete {
                                peer_id:   pid.clone(),
                                file_name: fname.clone(),
                            });
                            log(&etx, "OK", format!("{fname} written from local data"));
                        }
                        Err(e) => {
                            log(&etx, "ERROR", format!("Reassembly failed for {fname}: {e}"));
                        }
                    }
                })
            });

            // don't add to active — it's already done
            continue;
        }

        // ── Case B: partial or full download needed ───────────────────────
        log(
            event_tx,
            "INFO",
            format!(
                "⬇  {} — {} chunks, {} KB",
                pf.file_name,
                pf.total_chunks,
                pf.file_size / 1024
            ),
        );

        let _ = event_tx.send(GuiEvent::TransferStarted {
            peer_id:      pid_str.to_string(),
            file_name:    pf.file_name.clone(),
            total_chunks: pf.total_chunks,
            file_size:    pf.file_size,
        });

        // request only the chunks we don't have
        let count = needed.len().min(WINDOW);
        for &ci in needed.iter().take(count) {
            swarm.behaviour_mut().rr.send_request(
                &peer,
                SyncMessage::ChunkRequest {
                    file_name:   pf.file_name.clone(),
                    chunk_index: ci,
                },
            );
        }
        // next_request should skip past any locally-filled chunks
        ts.next_request = needed.get(count).copied().unwrap_or(total);

        dl.active.insert(pf.file_name.clone(), ts);
    }
}


// =============================================================================
// Helpers
// =============================================================================

/// Check whether a relative path (forward-slash separated) exists under sync_root.
/// e.g. rel = "photos/img.jpg"  →  checks sync_root/photos/img.jpg
fn rel_path_exists(sync_root: &std::path::PathBuf, rel: &str) -> bool {
    let mut p = sync_root.clone();
    for component in rel.split('/') {
        p.push(component);
    }
    p.exists()
}

fn log(tx: &mpsc::UnboundedSender<GuiEvent>, level: &str, message: String) {
    let _ = tx.send(GuiEvent::Log {
        level: level.into(),
        message,
    });
}

async fn folder_status(state: &Arc<Mutex<AppState>>) -> (bool, String) {
    let st = state.lock().await;
    (st.sync_path.is_some(), st.node_name.clone())
}

async fn peer_display_name(state: &Arc<Mutex<AppState>>, pid_str: &str) -> String {
    let st = state.lock().await;
    st.peer_names
        .get(pid_str)
        .cloned()
        .unwrap_or_else(|| format!("Node-{}", short_id(pid_str)))
}
