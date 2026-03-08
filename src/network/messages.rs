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

    FileDeleted {
        file_name: String,
    },

    TransferComplete,

    Empty,

    Error {
        message: String,
    },
}
