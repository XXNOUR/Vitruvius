// src/gui/types.rs
//
// All types that cross the WebSocket boundary between the browser and the backend.
//   GuiCommand  — browser → backend  (user actions)
//   GuiEvent    — backend → browser  (state updates, log lines, transfer progress)
//   GuiFileInfo — one row in a FolderListing

use serde::{Deserialize, Serialize};

// ─── Browser → Backend ────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum GuiCommand {
    /// Set (or change) the local sync folder
    SetFolder {
        path: String,
    },
    /// Dial a peer — addr is optional when mDNS already knows it
    DialPeer {
        peer_id: String,
        addr: Option<String>,
    },
    /// Manually request the file list from a connected peer
    RequestSync {
        peer_id: String,
    },
    /// Close the connection to a peer
    Disconnect {
        peer_id: String,
    },

    ApprovePeer {
        peer_id: String,
    },
    DenyPeer {
        peer_id: String,
    },

    /// Decrypt a `<name>.vit` file from the sync folder out to a plaintext
    /// destination on disk. Used by the GUI's "Decrypt File" button.
    DecryptFile {
        /// Logical filename (no `.vit` suffix); resolved against the current
        /// sync folder.
        name: String,
        /// Absolute destination path for the decrypted plaintext.
        dest: String,
    },
    /// Revoke the stored TOFU key for a peer — they must re-approve on next connect.
    RevokeKey {
        peer_id: String,
    },
    /// Toggle vault mode (at-rest .vit encryption) at runtime.
    /// If enabled and no vault key exists, one is auto-generated.
    SetVaultMode {
        enabled: bool,
    },
}

// ─── Backend → Browser ────────────────────────────────────────────────────────

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum GuiEvent {
    /// Sent once on startup — this node's own identity
    Identity { peer_id: String, node_name: String },
    /// A peer appeared on the LAN (mDNS discovery)
    PeerDiscovered {
        peer_id: String,
        addr: String,
        node_name: String,
    },
    /// TCP connection to a peer is open
    PeerConnected { peer_id: String, node_name: String },
    /// A peer connection was closed
    PeerDisconnected { peer_id: String },
    /// An outgoing dial attempt failed
    DialFailed {
        peer_id: Option<String>,
        error: String,
    },
    /// The contents of the local sync folder (sent after SetFolder and on reconnect)
    FolderListing { files: Vec<GuiFileInfo> },
    /// A new file transfer has started
    TransferStarted {
        peer_id: String,
        file_name: String,
        total_chunks: usize,
        file_size: u64,
    },
    /// One chunk was received (verified = BLAKE3 hash matched)
    ChunkReceived {
        peer_id: String,
        file_name: String,
        chunk_index: usize,
        total_chunks: usize,
        verified: bool,
    },
    /// All chunks received, file written to disk successfully
    TransferComplete { peer_id: String, file_name: String },
    /// The remote peer's folder is empty or not set
    RemoteEmpty { peer_id: String },
    // sends when the folder is set ,  to later on , set up the watcher
    /// A protocol-level error message from a peer
    PeerError { peer_id: String, message: String },

    PeerApprovalRequired {
        peer_id: String,
        display_name: String,
    },
    /// A log line — mirrors the tracing output into the GUI console
    Log { level: String, message: String },

    /// Snapshot of the node's zero-knowledge posture. Sent once per WS
    /// connect and again whenever a peer key is established. The GUI uses
    /// this to render the security status pill in the header.
    ///
    /// Zero-knowledge is about the WIRE, not the disk:
    ///   wire_encrypted    = true when this node has a transport key for at
    ///                        least one peer (TOFU or --key-path). Every chunk
    ///                        and manifest sent to that peer is AEAD-encrypted.
    ///   encrypted_protocol= true when the encrypted manifest/chunk variants
    ///                        are enabled (filenames/hashes never cross the wire
    ///                        in the clear).
    ///   key_fingerprint   = short BLAKE3 fingerprint of the active transport key.
    VaultStatus {
        /// True when at least one peer transport key is established.
        wire_encrypted: bool,
        /// True when encrypted manifest + AAD chunk variants are enabled.
        encrypted_protocol: bool,
        /// Short hex fingerprint of the current transport key ("—" = no key yet).
        key_fingerprint: String,
    },
    /// A peer's TOFU key was successfully revoked.
    PeerKeyRevoked { peer_id: String },
    /// Vault mode (at-rest .vit encryption) was toggled at runtime.
    VaultModeChanged { vault_mode: bool },
    /// The list of all peer IDs with stored TOFU keys.
    /// Sent once on WS connect and again after any revocation.
    TrustedPeers { peer_ids: Vec<String> },
    /// A vault file was successfully decrypted.
    DecryptComplete {
        name: String,
        dest: String,
        size: u64,
    },
}

// ─── One file in a FolderListing ─────────────────────────────────────────────

#[derive(Serialize, Debug, Clone)]
pub struct GuiFileInfo {
    pub name: String,
    pub size: u64,
    pub chunks: usize,
    /// True when the file is stored as a `*.vit` vault blob on disk.
    /// The GUI uses this flag to render the DECRYPT button.
    pub is_vault: bool,
}
