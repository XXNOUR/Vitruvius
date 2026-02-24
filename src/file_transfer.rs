// src/file_transfer.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File as StdFile, create_dir_all};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks

/// Messages for file transfer protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileMessage {
    /// Request to send a file
    RequestFile { name: String },

    /// Metadata about the file being sent
    Metadata {
        name: String,
        size: u64,
        total_chunks: u32,
    },

    /// A chunk of file data
    Chunk {
        file_name: String,
        index: u32,
        data: Vec<u8>,
    },

    /// Signal that transfer is complete
    Complete { file_name: String },

    /// Acknowledgment
    Ack { file_name: String },
}

/// State for tracking incoming file transfers
pub struct ReceiveState {
    pub file_name: String,
    pub total_chunks: u32,
    pub received_chunks: HashMap<u32, Vec<u8>>,
    pub output_path: PathBuf,
}

impl ReceiveState {
    pub fn new(file_name: String, total_chunks: u32, output_dir: &Path) -> Self {
        Self {
            file_name: file_name.clone(),
            total_chunks,
            received_chunks: HashMap::new(),
            output_path: output_dir.join(file_name),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.received_chunks.len() == self.total_chunks as usize
    }

    pub fn add_chunk(&mut self, index: u32, data: Vec<u8>) {
        self.received_chunks.insert(index, data);
        info!(
            "Received chunk {}/{} for file '{}'",
            self.received_chunks.len(),
            self.total_chunks,
            self.file_name
        );
    }

    pub fn write_to_disk(&self) -> Result<()> {
        info!("Writing file '{}' to disk", self.file_name);

        // Create parent directory if needed
        if let Some(parent) = self.output_path.parent() {
            create_dir_all(parent)?;
        }

        let mut file = StdFile::create(&self.output_path)?;

        // Write chunks in order
        for i in 0..self.total_chunks {
            if let Some(chunk_data) = self.received_chunks.get(&i) {
                file.write_all(chunk_data)?;
            } else {
                error!("Missing chunk {} for file '{}'", i, self.file_name);
                return Err(anyhow::anyhow!("Missing chunk {}", i));
            }
        }

        file.sync_all()?;
        info!(
            "File '{}' written successfully to {}",
            self.file_name,
            self.output_path.display()
        );

        Ok(())
    }
}

/// Chunk a file into fixed-size pieces
pub fn chunk_file(path: &Path) -> Result<Vec<Vec<u8>>> {
    let mut file = StdFile::open(path)?;
    let mut chunks = Vec::new();

    loop {
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let bytes_read = file.read(&mut buffer)?;

        if bytes_read == 0 {
            break; // EOF
        }

        buffer.truncate(bytes_read);
        chunks.push(buffer);
    }

    info!(
        "Chunked file '{}' into {} chunks",
        path.display(),
        chunks.len()
    );
    Ok(chunks)
}

/// Send a file to a peer
/// Returns a list of FileMessage to send over the network
pub fn prepare_file_transfer(file_path: &Path) -> Result<Vec<FileMessage>> {
    info!("Preparing to send file: {}", file_path.display());

    // Get file metadata
    let metadata = std::fs::metadata(file_path)?;
    let file_size = metadata.len();
    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid file name"))?
        .to_string_lossy()
        .to_string();

    // Chunk the file
    let chunks = chunk_file(file_path)?;
    let total_chunks = chunks.len() as u32;

    let mut messages = Vec::new();

    // 1. Send metadata first
    messages.push(FileMessage::Metadata {
        name: file_name.clone(),
        size: file_size,
        total_chunks,
    });

    // 2. Send each chunk
    for (index, chunk_data) in chunks.into_iter().enumerate() {
        messages.push(FileMessage::Chunk {
            file_name: file_name.clone(),
            index: index as u32,
            data: chunk_data,
        });
    }

    // 3. Send complete message
    messages.push(FileMessage::Complete {
        file_name: file_name.clone(),
    });

    info!(
        "Prepared {} messages for file transfer (1 metadata + {} chunks + 1 complete)",
        messages.len(),
        total_chunks
    );

    Ok(messages)
}

/// Handle an incoming file message
/// Returns Some(file_path) when transfer is complete
pub fn handle_file_message(
    message: FileMessage,
    receive_states: &mut HashMap<String, ReceiveState>,
    output_dir: &Path,
) -> Result<Option<PathBuf>> {
    match message {
        FileMessage::Metadata {
            name,
            size,
            total_chunks,
        } => {
            info!(
                "Receiving file '{}' ({} bytes, {} chunks)",
                name, size, total_chunks
            );

            let state = ReceiveState::new(name.clone(), total_chunks, output_dir);
            receive_states.insert(name, state);

            Ok(None)
        }

        FileMessage::Chunk {
            file_name,
            index,
            data,
        } => {
            if let Some(state) = receive_states.get_mut(&file_name) {
                state.add_chunk(index, data);

                // Check if complete
                if state.is_complete() {
                    info!("All chunks received for file '{}'", file_name);
                    state.write_to_disk()?;
                    let path = state.output_path.clone();
                    receive_states.remove(&file_name);
                    return Ok(Some(path));
                }
            } else {
                warn!("Received chunk for unknown file '{}' - ignoring", file_name);
            }

            Ok(None)
        }

        FileMessage::Complete { file_name } => {
            info!("Transfer complete signal received for '{}'", file_name);
            // Already handled in Chunk handler when all chunks arrive
            Ok(None)
        }

        FileMessage::Ack { file_name } => {
            info!("Acknowledgment received for file '{}'", file_name);
            Ok(None)
        }

        FileMessage::RequestFile { name } => {
            info!("Received request for file '{}'", name);
            // You'll handle this in main.rs when you want to respond to requests
            Ok(None)
        }
    }
}
