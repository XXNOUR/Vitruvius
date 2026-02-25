// src/storage.rs
use anyhow::Result;
use libp2p::PeerId;
use std::fs;
use std::path::PathBuf;
use tracing::info;

use crate::network::SyncMessage; // Import from network.rs

/// Read a file from sync directory and prepare it for sending
pub async fn send_file(peer_id: PeerId, sync_path: &PathBuf) -> Result<SyncMessage> {
    info!("Preparing file from {:?} for {}", sync_path, peer_id);

    let mut response = SyncMessage {
        file_name: "EMPTY".into(),
        content: None,
    };

    if let Ok(entries) = fs::read_dir(sync_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().unwrap().to_str().unwrap().to_string();

                if let Ok(data) = fs::read(&path) {
                    info!("Reading file: {} ({} bytes)", name, data.len());
                    response = SyncMessage {
                        file_name: name,
                        content: Some(data),
                    };
                    break;
                }
            }
        }
    }

    Ok(response)
}

/// Write received file data to disk
pub async fn receive_file(
    peer_id: PeerId,
    response: SyncMessage,
    sync_path: &PathBuf,
) -> Result<()> {
    info!("Processing response from {}", peer_id);

    if let Some(data) = response.content {
        if response.file_name != "EMPTY" {
            let dest = sync_path.join(&response.file_name);
            fs::write(&dest, &data)?;
            info!("File saved: {:?}", dest);
        } else {
            info!("Remote folder is empty");
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
//  WEEK 1 DAY 5-6: CHUNKING & HASHING (for next week)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
}

/// Split a file into chunks and compute BLAKE3 hash for each
pub fn chunk_file(file_path: &PathBuf) -> Result<Vec<Chunk>> {
    // TODO: Implement for Week 1 Day 5-6
    // 1. Read file
    // 2. Split into 1MB chunks
    // 3. Hash each chunk with BLAKE3
    // 4. Return vector of Chunk structs

    const CHUNK_SIZE: usize = 1024 * 1024; // 1MB

    info!("TODO: File chunking not yet implemented");
    Ok(vec![])
}

/// Verify a chunk's integrity using BLAKE3 hash
pub fn verify_chunk(chunk: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(chunk);
    computed_hash.as_bytes() == expected_hash
}
