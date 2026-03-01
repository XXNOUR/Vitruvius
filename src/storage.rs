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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
}

impl Chunk {
    fn new(index: usize, data: Vec<u8>, hash: [u8; 32]) -> Self {
        Chunk {
            index: index,
            data: data,
            hash: hash,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub file_name: String,
    pub total_chunks: usize,
    pub file_size: u64,
    pub chunk_hashes: Vec<[u8; 32]>,
}

pub struct FileTransferState {
    pub metadata: Option<FileMetadata>,
    pub received_chunks: HashMap<usize, Vec<u8>>,
    pub file_path: PathBuf,
}

impl FileTransferState {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            metadata: None,
            received_chunks: HashMap::new(),
            file_path,
        }
    }
}

/// Split a file into chunks and compute BLAKE3 hash for each  
pub fn chunk_file(file_path: &PathBuf) -> Result<(Vec<Chunk>, FileMetadata)> {
    const CHUNK_SIZE: usize = 1024 * 1024; // 1MB  

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

        chunks.push(Chunk::new(index, buffer.clone(), hash_bytes));
        chunk_hashes.push(hash_bytes);
        index += 1;
    }

    let file_name = file_path.file_name().unwrap().to_str().unwrap().to_string();

    let metadata = FileMetadata {
        file_name,
        total_chunks: chunks.len(),
        file_size,
        chunk_hashes,
    };

    info!("✂️  Chunked file into {} chunks", chunks.len());
    Ok((chunks, metadata))
}

/// Get file metadata without loading all chunks into memory  
pub async fn get_file_metadata(sync_path: &PathBuf) -> Result<SyncMessage> {
    info!("Getting file metadata from {:?}", sync_path);

    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let (_, metadata) = chunk_file(&path)?;
                return Ok(SyncMessage::Metadata {
                    file_name: metadata.file_name,
                    total_chunks: metadata.total_chunks,
                    file_size: metadata.file_size,
                    chunk_hashes: metadata.chunk_hashes,
                });
            }
        }
    }

    Ok(SyncMessage::Empty)
}

/// Get a specific chunk by index  
pub async fn get_chunk(sync_path: &PathBuf, chunk_index: usize) -> Result<SyncMessage> {
    info!("Getting chunk {} from {:?}", chunk_index, sync_path);

    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let (chunks, _) = chunk_file(&path)?;
                if let Some(chunk) = chunks.get(chunk_index) {
                    return Ok(SyncMessage::ChunkResponse {
                        chunk_index: chunk.index,
                        data: chunk.data.clone(),
                        hash: chunk.hash,
                    });
                } else {
                    return Ok(SyncMessage::Error {
                        message: format!("Chunk index {} out of range", chunk_index),
                    });
                }
            }
        }
    }

    Ok(SyncMessage::Error {
        message: "File not found".to_string(),
    })
}

/// Verify a chunk's integrity using BLAKE3 hash  
pub fn verify_chunk(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(data);
    computed_hash.as_bytes() == expected_hash
}

/// Process received chunk and check if transfer is complete  
pub async fn process_received_chunk(
    peer_id: PeerId,
    chunk_index: usize,
    data: Vec<u8>,
    hash: [u8; 32],
    transfer_state: &mut FileTransferState,
) -> Result<bool> {
    info!("Received chunk {} from {}", chunk_index, peer_id);

    // Verify chunk integrity
    if !verify_chunk(&data, &hash) {
        error!("Chunk {} failed verification", chunk_index);
        return Ok(false);
    }

    // Store chunk
    transfer_state.received_chunks.insert(chunk_index, data);

    // Check if we have all chunks
    if let Some(metadata) = &transfer_state.metadata {
        if transfer_state.received_chunks.len() == metadata.total_chunks {
            // Reassemble file
            reassemble_file(transfer_state)?;
            info!("✅ File transfer completed: {}", metadata.file_name);
            return Ok(true);
        }
    }

    Ok(false)
}

/// Reassemble file from received chunks  
fn reassemble_file(transfer_state: &FileTransferState) -> Result<()> {
    let metadata = transfer_state
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No metadata available"))?;

    let output_path = transfer_state.file_path.join(&metadata.file_name);
    let mut file = File::create(&output_path)?;

    // Write chunks in order
    for i in 0..metadata.total_chunks {
        if let Some(chunk_data) = transfer_state.received_chunks.get(&i) {
            file.write_all(chunk_data)?;
        } else {
            return Err(anyhow::anyhow!("Missing chunk {}", i));
        }
    }

    file.sync_all()?;
    info!("File reassembled: {:?}", output_path);
    Ok(())
}
