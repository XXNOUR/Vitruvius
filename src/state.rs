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
    pub known_addrs: HashMap<String, String>,
    /// set of peers with an open TCP connection right now
    pub connected_peers: HashSet<PeerId>,
    /// the local folder this node is syncing (None until the user sets it)
    pub sync_path: Option<PathBuf>,
    /// this device's human-readable hostname
    pub node_name: String,
    /// peer_id string → hostname, learned from FolderAnnouncement / Manifest
    pub peer_names: HashMap<String, String>,
    /// peers we have already sent a FolderAnnouncement to in this session.
    /// Prevents the ping-pong loop: A announces → B announces back → A announces → …
    /// Cleared when the sync folder changes (so re-announcing is intentional).
    pub announced_to: HashSet<PeerId>,
    pub writing_files: HashSet<PathBuf>,
    pub deleting_files: HashSet<PathBuf>,
    pub recently_notified: HashMap<String, std::time::Instant>,

    /// ChaCha20-Poly1305 encryption key shared by all peers in this sync group.
    /// None  → plaintext mode (useful for LAN testing without key setup).
    /// Some  → all chunk data is encrypted before sending, decrypted on receipt.
    /// The key is loaded from disk at startup via --key-path and never changes
    /// at runtime. All peers must use the same key file.
    pub encryption_key: Option<[u8; 32]>,
}

impl AppState {
    pub fn new(node_name: String) -> Self {
        Self {
            known_addrs: HashMap::new(),
            connected_peers: HashSet::new(),
            sync_path: None,
            node_name,
            peer_names: HashMap::new(),
            announced_to: HashSet::new(),
            writing_files: HashSet::new(),
            deleting_files: HashSet::new(),
            recently_notified: HashMap::new(),
            encryption_key: None,
        }
    }

    /// Convenience: returns true if this node is running in encrypted mode.
    pub fn is_encrypted(&self) -> bool {
        self.encryption_key.is_some()
    }
}

// ─── Determine this device's display name ────────────────────────────────────

pub fn get_node_name() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        let h = h.trim().to_string();
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim().to_string();
        if !h.is_empty() {
            return h;
        }
    }
    "Vitruvius-Node".to_string()
}

// ─── Short display version of a PeerId for logs ──────────────────────────────

pub fn short_id(peer_id: &str) -> String {
    let s = peer_id.strip_prefix("12D3KooW").unwrap_or(peer_id);
    s[..s.len().min(8)].to_string()
}
