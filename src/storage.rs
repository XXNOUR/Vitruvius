// src/storage.rs
use anyhow::Result;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use tracing::{error, info};

use crate::network::SyncMessage;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
}

impl Chunk {
    fn new(index: usize, data: Vec<u8>, hash: [u8; 32]) -> Self {
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

pub fn chunk_file(file_path: &PathBuf) -> Result<(Vec<Chunk>, FileMetadata)> {
    const CHUNK_SIZE: usize = 1024 * 1024;

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

    let file_name = file_path.file_name().unwrap().to_str().unwrap().to_string();

    let metadata = FileMetadata {
        file_name,
        total_chunks: chunks.len(),
        file_size,
        chunk_hashes,
    };

    info!("Chunked file into {} chunks", chunks.len());
    Ok((chunks, metadata))
}

pub async fn get_file_list(sync_path: &PathBuf) -> Result<Vec<String>> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    files.push(name.to_str().unwrap().to_string());
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

    let (_, metadata) = chunk_file(&file_path)?;

    Ok(SyncMessage::Metadata {
        file_name: metadata.file_name,
        total_chunks: metadata.total_chunks,
        file_size: metadata.file_size,
        chunk_hashes: metadata.chunk_hashes,
    })
}

pub async fn get_chunk(
    sync_path: &PathBuf,
    file_name: &str,
    chunk_index: usize,
) -> Result<SyncMessage> {
    info!("Getting chunk {} for file: {}", chunk_index, file_name);

    let file_path = sync_path.join(file_name);

    if !file_path.exists() {
        return Ok(SyncMessage::Error {
            message: format!("File '{}' not found", file_name),
        });
    }

    let (chunks, _) = chunk_file(&file_path)?;

    if let Some(chunk) = chunks.get(chunk_index) {
        Ok(SyncMessage::ChunkResponse {
            chunk_index: chunk.index,
            data: chunk.data.clone(),
            hash: chunk.hash,
        })
    } else {
        Ok(SyncMessage::Error {
            message: format!("Chunk {} out of range", chunk_index),
        })
    }
}

pub fn verify_chunk(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(data);
    computed_hash.as_bytes() == expected_hash
}

pub async fn process_received_chunk(
    peer_id: PeerId,
    chunk_index: usize,
    data: Vec<u8>,
    hash: [u8; 32],
    transfer_state: &mut FileTransferState,
) -> Result<bool> {
    info!("Processing chunk {} from {}", chunk_index, peer_id);

    if !verify_chunk(&data, &hash) {
        error!("Chunk {} failed verification", chunk_index);
        return Ok(false);
    }

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

    let output_path = transfer_state.file_path.join(&metadata.file_name);
    let mut file = File::create(&output_path)?;

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
