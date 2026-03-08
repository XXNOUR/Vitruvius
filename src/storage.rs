// src/storage.rs
use anyhow::Result;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};

use crate::network::SyncMessage;

const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
}

impl Chunk {
    pub fn new(index: usize, data: Vec<u8>, hash: [u8; 32]) -> Self {
        Chunk { index, data, hash }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub file_name: String,
    pub total_chunks: usize,
    pub file_size: u64,
    pub chunk_hashes: Vec<[u8; 32]>,
}

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
    pub failed_chunks: HashSet<usize>,
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
            failed_chunks: HashSet::new(),
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

// Need to import HashSet
use std::collections::HashSet;

/// Chunk a file into smaller pieces with hashes
pub fn chunk_file(file_path: &PathBuf) -> Result<(Vec<Chunk>, FileMetadata)> {
    let file = File::open(file_path)?;
    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunk_hashes: Vec<[u8; 32]> = Vec::new();
    let mut index = 0;

    loop {
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let bytes_read = reader.read(&mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        buffer.truncate(bytes_read);
        let hash = blake3::hash(&buffer);
        let hash_bytes = hash.into();

        chunks.push(Chunk::new(index, buffer, hash_bytes));
        chunk_hashes.push(hash_bytes);
        index += 1;
    }

    // Handle empty files
    if chunks.is_empty() {
        let hash = blake3::hash(&[]);
        let hash_bytes = hash.into();
        chunks.push(Chunk::new(0, vec![], hash_bytes));
        chunk_hashes.push(hash_bytes);
    }

    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let metadata = FileMetadata {
        file_name,
        total_chunks: chunks.len(),
        file_size,
        chunk_hashes,
    };

    info!(
        "Chunked file '{}' into {} chunks ({} bytes)",
        metadata.file_name,
        chunks.len(),
        file_size
    );
    Ok((chunks, metadata))
}

pub async fn get_file_list(sync_path: &PathBuf) -> Result<Vec<String>> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                // Skip hidden files and temporary files
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if !name.starts_with('.') && !name.ends_with(".tmp") {
                        files.push(name.to_string());
                    }
                }
            }
        }
    }

    info!("Found {} files in directory", files.len());
    Ok(files)
}

pub async fn get_file_metadata(sync_path: &PathBuf, file_name: &str) -> Result<SyncMessage> {
    info!("Getting metadata for file: {}", file_name);

    let file_path = sync_path.join(file_name);

    if !file_path.exists() || !file_path.is_file() {
        return Ok(SyncMessage::Error {
            message: format!("File '{}' not found", file_name),
        });
    }

    let canonical = fs::canonicalize(&file_path)?;
    if !canonical.starts_with(sync_path) {
        warn!("Path traversal attempt detected: {}", file_name);
        return Ok(SyncMessage::Error {
            message: "Invalid file path".to_string(),
        });
    }

    let (_, metadata) = chunk_file(&file_path)?;

    Ok(SyncMessage::Metadata {
        file_name: metadata.file_name,
        total_chunks: metadata.total_chunks,
        file_size: metadata.file_size,
        chunk_hashes: metadata.chunk_hashes,
    })
}

/// Verify chunk integrity using Blake3 hash
pub fn verify_chunk(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(data);
    computed_hash.as_bytes() == expected_hash
}

/// Process a received chunk
/// Returns Ok(true) if file transfer is complete, Ok(false) if more chunks needed
pub async fn process_received_chunk(
    peer_id: PeerId,
    chunk_index: usize,
    data: Vec<u8>,
    hash: [u8; 32],
    transfer_state: &mut FileTransferState,
) -> Result<bool> {
    info!(
        "Processing chunk {} from {} (file progress: {:.1}%)",
        chunk_index,
        peer_id,
        transfer_state.progress_percent()
    );

    // Decrement in-flight count
    if transfer_state.chunks_in_flight > 0 {
        transfer_state.chunks_in_flight -= 1;
    }

    // Get expected hash from metadata
    let expected_hash = transfer_state
        .metadata
        .as_ref()
        .and_then(|m| m.chunk_hashes.get(chunk_index))
        .copied();

    if let Some(expected) = expected_hash {
        if !verify_chunk(&data, &expected) {
            error!("Chunk {} failed hash verification", chunk_index);
            transfer_state.failed_chunks.insert(chunk_index);
            return Ok(false);
        }
    } else {
        warn!(
            "No expected hash for chunk {}, accepting anyway",
            chunk_index
        );
    }

    transfer_state.failed_chunks.remove(&chunk_index);

    transfer_state.received_chunks.insert(chunk_index, data);

    if let Some(metadata) = &transfer_state.metadata {
        if transfer_state.received_chunks.len() == metadata.total_chunks {
            reassemble_file(transfer_state)?;
            info!("File transfer completed: {}", metadata.file_name);
            return Ok(true);
        }
    }

    Ok(false)
}

fn reassemble_file(transfer_state: &FileTransferState) -> Result<()> {
    let metadata = transfer_state
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No metadata available"))?;

    // Write to temporary file first, then rename (atomic write)
    let output_path = transfer_state.file_path.join(&metadata.file_name);
    let temp_path = output_path.with_extension("tmp");

    {
        let mut file = File::create(&temp_path)?;

        for i in 0..metadata.total_chunks {
            if let Some(chunk_data) = transfer_state.received_chunks.get(&i) {
                file.write_all(chunk_data)?;
            } else {
                return Err(anyhow::anyhow!("Missing chunk {}", i));
            }
        }

        file.sync_all()?;
    }

    // Atomic rename
    fs::rename(&temp_path, &output_path)?;
    info!("File reassembled: {:?}", output_path);

    Ok(())
}

/// Delete a file from the sync directory
pub fn delete_file(sync_path: &PathBuf, file_name: &str) -> Result<()> {
    let file_path = sync_path.join(file_name);

    // Security check: ensure file is within sync path
    if file_path.exists() {
        let canonical = fs::canonicalize(&file_path)?;
        if !canonical.starts_with(sync_path) {
            warn!(
                "Path traversal attempt detected during delete: {}",
                file_name
            );
            return Err(anyhow::anyhow!("Invalid file path"));
        }

        fs::remove_file(&file_path)?;
        info!("Deleted file: {}", file_name);
    }

    Ok(())
}
