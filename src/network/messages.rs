use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SyncMessage {
    MetaDataRequest {
        file_name: String,
    },

    ListFiles,

    FileList {
        file_names: Vec<String>,
    },

    Metadata {
        file_name: String,
        total_chunks: usize,
        file_size: u64,
        chunk_hashes: Vec<[u8; 32]>,
    },

    /// Extended metadata with version and delta info
    VersionedMetadata {
        file_name: String,
        file_hash: [u8; 32],
        parent_hash: Option<[u8; 32]>,
        total_chunks: usize,
        file_size: u64,
        chunk_hashes: Vec<[u8; 32]>,
        changed_chunks: Vec<usize>,
        timestamp: u64,
        author: String,
    },

    ChunkRequest {
        file_name: String,
        chunk_index: usize,
    },

    ChunkResponse {
        chunk_index: usize,
        data: Vec<u8>,
        hash: [u8; 32],
        file_name: String,
    },

    FileChanged {
        file_name: String,
    },

    /// File changed with version info for efficient delta detection
    FileChangedWithVersion {
        file_name: String,
        file_hash: [u8; 32],
        changed_chunk_count: usize,
        timestamp: u64,
    },

    FileDeleted {
        file_name: String,
    },

    /// A directory was created (relative path from sync root)
    DirectoryCreated {
        path: String,
    },

    /// A directory was removed
    DirectoryDeleted {
        path: String,
    },

    TransferComplete,

    Empty,

    Error {
        message: String,
    },
}
