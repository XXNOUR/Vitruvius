use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub file_name: String,
    pub total_chunks: usize,
    pub file_size: u64,
    pub chunk_hashes: Vec<[u8; 32]>,
}

/// Represents a version of a file with full history tracking
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileVersion {
    /// Hash of the entire file content
    pub file_hash: [u8; 32],
    /// Hash of the previous version (enables rollback/history chain)
    pub parent_hash: Option<[u8; 32]>,
    /// Metadata for this version
    pub metadata: FileMetadata,
    /// Timestamp when this version was created (unix seconds)
    pub timestamp: u64,
    /// Peer ID or identifier  that created this version
    pub author: String,
    /// Indices of chunks that changed from parent
    pub changed_chunks: Vec<usize>,
}

impl FileVersion {
    pub fn new(
        file_hash: [u8; 32],
        metadata: FileMetadata,
        author: String,
    ) -> Self {
        let total_chunks = metadata.total_chunks;
        Self {
            file_hash,
            parent_hash: None,
            metadata,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            author,
            changed_chunks: (0..total_chunks).collect(), // Initially all chunks
        }
    }

    /// Create a new version from a parent, with only changed chunks specified
    pub fn from_parent(
        file_hash: [u8; 32],
        metadata: FileMetadata,
        author: String,
        parent: &FileVersion,
        changed_chunks: Vec<usize>,
    ) -> Self {
        Self {
            file_hash,
            parent_hash: Some(parent.file_hash),
            metadata,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            author,
            changed_chunks,
        }
    }
}
