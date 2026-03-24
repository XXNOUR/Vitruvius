// src/state.rs
//
// Shared mutable state for the running node.
// Wrapped in Arc<Mutex<AppState>> and accessed from the swarm loop,
// the WebSocket handler, and the GUI command handler.

use libp2p::PeerId;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub struct AppState {
    /// peer_id string → last known multiaddr (populated by mDNS)
    pub known_addrs:     HashMap<String, String>,
    /// set of peers with an open TCP connection right now
    pub connected_peers: HashSet<PeerId>,
    /// the local folder this node is syncing (None until the user sets it)
    pub sync_path:       Option<PathBuf>,
    /// this device's human-readable hostname
    pub node_name:       String,
    /// peer_id string → hostname, learned from FolderAnnouncement / Manifest
    pub peer_names:      HashMap<String, String>,
    /// peers we have already sent a FolderAnnouncement to in this session.
    /// Prevents the ping-pong loop: A announces → B announces back → A announces → …
    /// Cleared when the sync folder changes (so re-announcing is intentional).
    pub announced_to:    HashSet<PeerId>,
}

impl AppState {
    pub fn new(node_name: String) -> Self {
        Self {
            known_addrs:     HashMap::new(),
            connected_peers: HashSet::new(),
            sync_path:       None,
            node_name,
            peer_names:      HashMap::new(),
            announced_to:    HashSet::new(),
        }
    }
}

// ─── Determine this device's display name ────────────────────────────────────

pub fn get_node_name() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        let h = h.trim().to_string();
        if !h.is_empty() { return h; }
    }
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim().to_string();
        if !h.is_empty() { return h; }
    }
    "Vitruvius-Node".to_string()
}

// ─── Short display version of a PeerId for logs ──────────────────────────────

pub fn short_id(peer_id: &str) -> String {
    let s = peer_id.strip_prefix("12D3KooW").unwrap_or(peer_id);
    s[..s.len().min(8)].to_string()
}
