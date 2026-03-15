use std::collections::HashMap;
use libp2p::PeerId;
use crate::network::SyncSession;
use crate::storage::FileTransferState;

/// Manages overall sync state
pub struct SyncState {
    pub current_session: Option<SyncSession>,
    pub is_initial_sync_complete: bool,
    pub transfer_states: HashMap<String, FileTransferState>,
    pub connected_peers: Vec<PeerId>,
    pub sync_target_peer: Option<PeerId>,
}

impl SyncState {
    pub fn new() -> Self {
        Self {
            current_session: None,
            is_initial_sync_complete: false,
            transfer_states: HashMap::new(),
            connected_peers: Vec::new(),
            sync_target_peer: None,
        }
    }
}

impl Default for SyncState {
    fn default() -> Self {
        Self::new()
    }
}
