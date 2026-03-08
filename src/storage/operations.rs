use anyhow::Result;
use libp2p::PeerId;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::chunk::chunk_file;
use super::transfer::FileTransferState;
use crate::network::SyncMessage;

/// Verify chunk integrity using Blake3 hash
pub fn verify_chunk(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let computed_hash = blake3::hash(data);
    computed_hash.as_bytes() == expected_hash
}

/// Get list of files in a directory
pub async fn get_file_list(sync_path: &PathBuf) -> Result<Vec<String>> {
    let mut files = Vec::new();

    fn recurse(dir: &PathBuf, base: &PathBuf, out: &mut Vec<String>) -> std::io::Result<()> {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(rel) = path.strip_prefix(base) {
                        if let Some(name) = rel.to_str() {
                            // Skip hidden/temporary
                            if !name.starts_with('.') && !name.ends_with(".tmp") {
                                out.push(name.to_string());
                            }
                        }
                    }
                } else if path.is_dir() {
                    recurse(&path, base, out)?;
                }
            }
        }
        Ok(())
    }

    recurse(sync_path, sync_path, &mut files)?;

    info!("Found {} files in directory (recursive)", files.len());
    Ok(files)
}

/// Get metadata for a file
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

/// Process a received chunk
/// Returns Ok(true) if file transfer is complete, Ok(false) if more chunks needed
pub async fn process_received_chunk(
    peer_id: PeerId,
    chunk_index: usize,
    data: Vec<u8>,
    _hash: [u8; 32],
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
    use std::io::Write;

    let metadata = transfer_state
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No metadata available"))?;

    // Write to temporary file first, then rename (atomic write)
    let output_path = transfer_state.file_path.join(&metadata.file_name);

    // ensure parent directory exists (handles nested relative paths)
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp_path = output_path.with_extension("tmp");

    {
        let mut file = std::fs::File::create(&temp_path)?;

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
