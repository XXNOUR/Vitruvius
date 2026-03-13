use std::collections::HashMap;
use std::path::PathBuf;
use super::metadata::{FileMetadata, FileVersion};

/// Tracks the state of an incoming file transfer
pub struct FileTransferState {
    pub metadata: Option<FileMetadata>,
    /// Current version being transferred
    pub current_version: Option<FileVersion>,
    /// Previous version for delta detection
    pub previous_version: Option<FileVersion>,
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
    /// Chunks that are actually needed (delta aware)
    pub required_chunks: std::collections::HashSet<usize>,
}

impl FileTransferState {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            metadata: None,
            current_version: None,
            previous_version: None,
            received_chunks: HashMap::new(),
            file_path,
            next_chunk_to_request: 0,
            chunks_in_flight: 0,
            chunks_expected: 0,
            failed_chunks: std::collections::HashSet::new(),
            required_chunks: std::collections::HashSet::new(),
        }
    }

    /// Calculate progress percentage
    pub fn progress_percent(&self) -> f32 {
        if self.required_chunks.is_empty() {
            return 0.0;
        }
        let received = self.received_chunks.iter()
            .filter(|(idx, _)| self.required_chunks.contains(idx))
            .count();
        (received as f32 / self.required_chunks.len() as f32) * 100.0
    }

    /// Set which chunks are required based on the current version
    pub fn set_required_chunks(&mut self) {
        if let Some(version) = &self.current_version {
            if self.previous_version.is_some() {
                // Delta mode: only changed chunks
                self.required_chunks = version.changed_chunks.iter().cloned().collect();
            } else {
                // Full sync: all chunks
                self.required_chunks = (0..version.metadata.total_chunks).collect();
            }
            self.chunks_expected = self.required_chunks.len();
        }
    }

    /// Check if transfer is complete (only required chunks)
    pub fn is_transfer_complete(&self) -> bool {
        !self.required_chunks.is_empty() &&
            self.received_chunks.iter()
                .filter(|(idx, _)| self.required_chunks.contains(idx))
                .count() == self.required_chunks.len()
    }
}
