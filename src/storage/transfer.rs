use std::collections::HashMap;
use std::path::PathBuf;
use super::metadata::FileMetadata;

/// Tracks the state of an incoming file transfer
pub struct FileTransferState {
    pub metadata: Option<FileMetadata>,
    pub received_chunks: HashMap<usize, Vec<u8>>,
    pub file_path: PathBuf,
    /// Next chunk index to request (for pipelining)
    pub next_chunk_to_request: usize,
    /// Number of chunks currently in-flight
    pub chunks_in_flight: usize,
    /// Total chunks expected
    pub chunks_expected: usize,
    /// Track which chunks failed verification for retry
    pub failed_chunks: std::collections::HashSet<usize>,
}

impl FileTransferState {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            metadata: None,
            received_chunks: HashMap::new(),
            file_path,
            next_chunk_to_request: 0,
            chunks_in_flight: 0,
            chunks_expected: 0,
            failed_chunks: std::collections::HashSet::new(),
        }
    }

    /// Calculate progress percentage
    pub fn progress_percent(&self) -> f32 {
        if self.chunks_expected == 0 {
            return 0.0;
        }
        (self.received_chunks.len() as f32 / self.chunks_expected as f32) * 100.0
    }
}
