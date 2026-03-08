use serde::{Deserialize, Serialize};
use anyhow::Result;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use tracing::info;

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

use super::metadata::FileMetadata;

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
