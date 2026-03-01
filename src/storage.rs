// src/storage.rs
use anyhow::Result;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use tracing::info;

use crate::network::SyncMessage;

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

/// Split a file into chunks and compute BLAKE3 hash for each
pub fn chunk_file(file_path: &PathBuf) -> Result<Vec<Chunk>> {
    const CHUNK_SIZE: usize = 1024 * 1024; // 1MB

    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut index = 0;

    loop {
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let bytes_read = reader.read(&mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        buffer.truncate(bytes_read);
        let hash = blake3::hash(&buffer);
        chunks.push(Chunk::new(index, buffer, hash.into()));
        index += 1;
    }

    info!("✂️  Chunked file into {} chunks", chunks.len());
    Ok(chunks)
}

/// Verify a chunk's integrity using BLAKE3 hash
pub fn verify_chunk(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(data);
    computed_hash.as_bytes() == expected_hash
}

/// Prepare a file for sending (chunks it and returns a single message)
pub async fn prepare_file_send(peer_id: PeerId, sync_path: &PathBuf) -> Result<SyncMessage> {
    info!("Preparing file from {:?} for {}", sync_path, peer_id);

    // Find first file in directory
    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let file_name = path.file_name().unwrap().to_str().unwrap().to_string();

                let file_size = path.metadata()?.len();

                // Chunk the file
                let chunks = chunk_file(&path)?;

                info!(
                    " Sending '{}' ({} bytes, {} chunks)",
                    file_name,
                    file_size,
                    chunks.len()
                );

                // Return single message with all chunks
                return Ok(SyncMessage::FileTransfer {
                    file_name,
                    total_chunks: chunks.len(),
                    file_size,
                    chunks,
                });
            }
        }
    }

    // No files found
    info!("📭 No files to send");
    Ok(SyncMessage::Empty)
}

/// Receive and write a file transfer message
pub async fn receive_file_transfer(
    peer_id: PeerId,
    file_name: String,
    chunks: Vec<Chunk>,
    file_size: u64,
    sync_path: &PathBuf,
) -> Result<()> {
    info!(
        " Receiving '{}' from {} ({} chunks, {} bytes)",
        file_name,
        peer_id,
        chunks.len(),
        file_size
    );

    let output_path = sync_path.join(&file_name);
    let mut file = File::create(&output_path)?;

    let mut verified_count = 0;
    let mut failed_count = 0;

    // Write chunks in order and verify each one
    for chunk in chunks {
        // Verify hash
        if verify_chunk(&chunk.data, &chunk.hash) {
            file.write_all(&chunk.data)?;
            verified_count += 1;
        } else {
            failed_count += 1;
            info!("Chunk {} failed verification", chunk.index);
        }
    }

    file.sync_all()?;

    if failed_count > 0 {
        info!(
            "File saved with {} verified chunks, {} failed",
            verified_count, failed_count
        );
    } else {
        info!(
            " File saved: {:?} ({} bytes, all {} chunks verified)",
            output_path, file_size, verified_count
        );
    }

    Ok(())
}
