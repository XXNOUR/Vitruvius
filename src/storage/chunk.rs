use serde::{Deserialize, Serialize};
use anyhow::Result;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use tracing::info;
use rayon::prelude::*;

const CHUNK_SIZE: usize = 16 * 1024 * 1024; // 16MB chunks for better Performance

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
    let reader = BufReader::with_capacity(256 * 1024, file);

    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Read all chunks first (still sequential I/O, but with larger buffer)
    let mut raw_chunks: Vec<Vec<u8>> = Vec::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut reader = reader;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        // Only clone the actual bytes read, not the full buffer
        raw_chunks.push(buffer[..bytes_read].to_vec());
    }

    // Handle empty files
    if raw_chunks.is_empty() {
        raw_chunks.push(vec![]);
    }

    // Parallel hashing using rayon
    let chunks: Vec<Chunk> = raw_chunks
        .into_par_iter()
        .enumerate()
        .map(|(index, data)| {
            let hash = blake3::hash(&data);
            Chunk::new(index, data, hash.into())
        })
        .collect();

    let chunk_hashes: Vec<[u8; 32]> = chunks.iter().map(|c| c.hash).collect();

    let metadata = FileMetadata {
        file_name: file_name.clone(),
        total_chunks: chunks.len(),
        file_size,
        chunk_hashes,
    };

    info!(
        "Chunked file '{}' into {} chunks ({} bytes)",
        file_name,
        chunks.len(),
        file_size
    );
    Ok((chunks, metadata))
}
