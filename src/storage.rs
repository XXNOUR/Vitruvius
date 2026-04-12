// src/storage.rs
//
// Key design decision: ALL file identifiers are RELATIVE PATHS from the sync root.
// Examples:  "report.pdf"  |  "photos/img001.jpg"  |  "src/utils/parser.cpp"
//
// This means:
//   - list_folder() walks the tree recursively and returns relative paths
//   - get_chunk()   opens  sync_root / relative_path
//   - reassemble()  writes sync_root / relative_path, creating parent dirs first
//
// The relative path is the identity of a file across both peers. If peer A has
// "photos/img001.jpg" at sync_root/photos/img001.jpg, then after sync peer B
// will have the exact same file at their_sync_root/photos/img001.jpg.
// The directory tree is preserved perfectly.
//
// ── Encryption design ─────────────────────────────────────────────────────────
//
// Chunk hashes in the manifest are always computed from PLAINTEXT.
// This means:
//   - The sender hashes plaintext in compute_file_metadata() — unchanged.
//   - The sender encrypts in get_chunk() just before sending — added here.
//   - The receiver decrypts in on_response() BEFORE hash verification — in handler.rs.
//   - reassemble() always writes plaintext to disk — unchanged.
//
// This ordering guarantees:
//   1. Integrity: hash verifies the plaintext content, not the encryption wrapper.
//   2. Zero-knowledge: relay nodes only ever see ciphertext in ChunkResponse.data.
//   3. Deduplication: identical plaintext blocks have the same hash regardless
//      of the nonce used to encrypt them, so the chunk index still works.

use anyhow::{Context, Result};
use libp2p::PeerId;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{error, info};

use crate::crypto;
use crate::network::{FileEntry, SyncMessage};

pub const CHUNK_SIZE: u64 = 512 * 1024; // 512 KB

// ─── Metadata for one file ────────────────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct FileMetadata {
    /// Relative path from the sync root, using forward slashes, e.g. "photos/img.jpg"
    pub file_name: String,
    pub total_chunks: usize,
    pub file_size: u64,
    /// Hashes of PLAINTEXT chunks — used for integrity verification and deduplication.
    /// These are always plaintext hashes even when encryption is enabled.
    pub chunk_hashes: Vec<[u8; 32]>,
}

// ─── State for one in-progress incoming transfer ──────────────────────────────
pub struct FileTransferState {
    pub metadata: Option<FileMetadata>,
    /// Decrypted plaintext chunks, indexed by chunk number.
    /// Chunks are decrypted in on_response() before being stored here.
    pub received_chunks: HashMap<usize, Vec<u8>>,
    pub sync_dir: PathBuf, // the sync root (not the file's parent)
    pub next_request: usize,
    pub last_activity: Instant,
}

impl FileTransferState {
    pub fn new(sync_dir: PathBuf) -> Self {
        Self {
            metadata: None,
            received_chunks: HashMap::new(),
            sync_dir,
            next_request: 0,
            last_activity: Instant::now(),
        }
    }

    pub fn missing_chunks(&self) -> Vec<usize> {
        let total = self.metadata.as_ref().map(|m| m.total_chunks).unwrap_or(0);
        (0..total)
            .filter(|i| !self.received_chunks.contains_key(i))
            .collect()
    }
}

// ─── Pending file (queued, not yet downloading) ───────────────────────────────
#[derive(Debug, Clone)]
pub struct PendingFile {
    pub file_name: String, // relative path
    pub total_chunks: usize,
    pub file_size: u64,
    pub chunk_hashes: Vec<[u8; 32]>,
}

impl From<&FileEntry> for PendingFile {
    fn from(fe: &FileEntry) -> Self {
        PendingFile {
            file_name: fe.file_name.clone(),
            total_chunks: fe.total_chunks,
            file_size: fe.file_size,
            chunk_hashes: fe.chunk_hashes.clone(),
        }
    }
}

// ─── Convert an absolute path to a relative path string ──────────────────────
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().and_then(|rel| {
        let s = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    })
}

// ─── Compute metadata for one file ────────────────────────────────────────────
// Hashes are always computed from plaintext — encryption happens only at send time.
fn compute_file_metadata(root: &Path, abs_path: &Path) -> Result<FileMetadata> {
    let file_name = relative_path(root, abs_path)
        .ok_or_else(|| anyhow::anyhow!("Cannot make relative path for {:?}", abs_path))?;

    let mut file = File::open(abs_path).with_context(|| format!("Cannot open {:?}", abs_path))?;
    let file_size = file.metadata()?.len();

    let total_chunks = ((file_size + CHUNK_SIZE - 1) / CHUNK_SIZE).max(1) as usize;
    let mut chunk_hashes = Vec::with_capacity(total_chunks);
    let mut buf = vec![0u8; CHUNK_SIZE as usize];
    for _ in 0..total_chunks {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // Hash plaintext — this is intentional and must not change.
        // The receiver verifies plaintext hashes after decryption.
        chunk_hashes.push(blake3::hash(&buf[..n]).into());
    }
    Ok(FileMetadata {
        file_name,
        total_chunks,
        file_size,
        chunk_hashes,
    })
}

// ─── Recursively walk a directory tree ────────────────────────────────────────
fn walk_dir(root: &Path, dir: &Path, results: &mut Vec<FileMetadata>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            error!("Cannot read dir {:?}: {}", dir, e);
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(root, &path, results);
        } else if path.is_file() {
            match compute_file_metadata(root, &path) {
                Ok(m) => results.push(m),
                Err(e) => error!("Skipping {:?}: {}", path, e),
            }
        }
    }
}

// ─── Scan directory tree recursively ──────────────────────────────────────────
pub async fn list_folder(sync_root: &PathBuf) -> Result<Vec<FileMetadata>> {
    let mut result = Vec::new();
    walk_dir(sync_root.as_path(), sync_root.as_path(), &mut result);
    result.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(result)
}

// ─── Build Manifest ───────────────────────────────────────────────────────────
// Manifest always contains plaintext hashes — no encryption involved here.
pub async fn get_manifest(sync_root: &PathBuf, node_name: &str) -> Result<SyncMessage> {
    let files = list_folder(sync_root).await?;
    if files.is_empty() {
        return Ok(SyncMessage::Empty);
    }
    let entries = files
        .into_iter()
        .map(|m| FileEntry {
            file_name: m.file_name,
            file_size: m.file_size,
            total_chunks: m.total_chunks,
            chunk_hashes: m.chunk_hashes,
        })
        .collect();
    Ok(SyncMessage::Manifest {
        node_name: node_name.to_string(),
        files: entries,
    })
}

// ─── Serve one chunk ──────────────────────────────────────────────────────────
//
// Encryption is applied here, just before the data leaves this device.
// If encryption_key is Some, the plaintext chunk is encrypted with ChaCha20-Poly1305
// and the ciphertext (nonce + ciphertext + tag) is sent as ChunkResponse.data.
// If encryption_key is None, plaintext is sent — backward-compatible plaintext mode.
//
// The hash field in ChunkResponse always carries the PLAINTEXT hash so the
// receiver can verify integrity after decrypting. The receiver must decrypt first,
// then check the hash.
pub async fn get_chunk(
    sync_root: &PathBuf,
    rel_path: &str,
    chunk_index: usize,
    encryption_key: Option<&[u8; 32]>,
) -> Result<SyncMessage> {
    // Sanitise: reject any path that tries to escape the sync root
    if rel_path.contains("..") || rel_path.starts_with('/') {
        return Ok(SyncMessage::Error {
            message: format!("Rejected unsafe path: {rel_path}"),
        });
    }

    // Convert forward-slash relative path to OS path
    let mut file_path = sync_root.clone();
    for component in rel_path.split('/') {
        file_path.push(component);
    }

    let mut file =
        File::open(&file_path).with_context(|| format!("Cannot open {:?}", file_path))?;

    let offset = chunk_index as u64 * CHUNK_SIZE;
    file.seek(SeekFrom::Start(offset))?;

    let mut buf = vec![0u8; CHUNK_SIZE as usize];
    let n = file.read(&mut buf)?;
    if n == 0 {
        return Ok(SyncMessage::Error {
            message: format!("Chunk {} past end of {}", chunk_index, rel_path),
        });
    }
    buf.truncate(n);

    // Hash is always computed from plaintext — receiver verifies after decryption.
    let hash: [u8; 32] = blake3::hash(&buf).into();

    // Encrypt if a key is configured — this is the zero-knowledge boundary.
    // After this point, buf contains ciphertext that relay nodes cannot read.
    let data = match encryption_key {
        Some(key) => crypto::encrypt(key, &buf)
            .map_err(|e| anyhow::anyhow!("Chunk encryption failed: {e}"))?,
        None => buf,
    };

    Ok(SyncMessage::ChunkResponse {
        file_name: rel_path.to_string(),
        chunk_index,
        data,
        hash, // plaintext hash — always
    })
}

// ─── Verify chunk hash ────────────────────────────────────────────────────────
// Call this on the PLAINTEXT (after decryption) — not on ciphertext.
pub fn verify_chunk(data: &[u8], expected: &[u8; 32]) -> bool {
    blake3::hash(data).as_bytes() == expected
}

// ─── Reassemble file from chunks ──────────────────────────────────────────────
// Chunks in FileTransferState are always plaintext — decryption happens in handler.rs.
pub async fn reassemble(ts: &FileTransferState) -> Result<PathBuf> {
    let meta = ts
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No metadata during reassembly"))?;

    let mut out_path = ts.sync_dir.clone();
    for component in meta.file_name.split('/') {
        out_path.push(component);
    }

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Cannot create dirs {:?}", parent))?;
    }

    let mut out =
        File::create(&out_path).with_context(|| format!("Cannot create {:?}", out_path))?;

    for i in 0..meta.total_chunks {
        match ts.received_chunks.get(&i) {
            Some(d) => out.write_all(d)?,
            None => {
                return Err(anyhow::anyhow!(
                    "Missing chunk {} during reassembly of {}",
                    i,
                    meta.file_name
                ))
            }
        }
    }
    out.sync_all()?;
    info!("Written: {:?}", out_path);
    Ok(out_path)
}

// ─── Build a hash→(file, chunk) index for deduplication ──────────────────────
// Hashes are always plaintext hashes — consistent with chunk_hashes in metadata.
pub async fn build_chunk_index(
    sync_root: &PathBuf,
) -> HashMap<[u8; 32], (String, usize)> {
    let mut index = HashMap::new();
    let files = match list_folder(sync_root).await {
        Ok(f) => f,
        Err(_) => return index,
    };
    for file in files {
        for (chunk_index, hash) in file.chunk_hashes.iter().enumerate() {
            index.entry(*hash).or_insert_with(|| {
                (file.file_name.clone(), chunk_index)
            });
        }
    }
    index
}

pub async fn read_local_chunk(
    sync_root: &PathBuf,
    rel_path: &str,
    chunk_index: usize,
) -> Option<Vec<u8>> {
    // Read plaintext — no encryption key needed here because local files are
    // always stored as plaintext on disk. Encryption only happens in transit.
    match get_chunk(sync_root, rel_path, chunk_index, None).await {
        Ok(SyncMessage::ChunkResponse { data, .. }) => Some(data),
        _ => None,
    }
}

// ─── Legacy wrapper ───────────────────────────────────────────────────────────
#[allow(dead_code)]
pub async fn process_chunk(
    _peer_id: PeerId,
    chunk_index: usize,
    data: Vec<u8>,
    ts: &mut FileTransferState,
) -> Result<bool> {
    ts.received_chunks.insert(chunk_index, data);
    let total = ts.metadata.as_ref().map(|m| m.total_chunks).unwrap_or(0);
    if ts.received_chunks.len() < total {
        return Ok(false);
    }
    reassemble(ts).await?;
    Ok(true)
}
