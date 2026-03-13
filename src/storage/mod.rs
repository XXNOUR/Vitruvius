pub mod chunk;
pub mod metadata;
pub mod transfer;
pub mod operations;

pub use chunk::{Chunk, chunk_file, compute_file_hash, detect_changed_chunks, create_file_version, create_file_version_from_parent};
pub use metadata::{FileMetadata, FileVersion};
pub use transfer::FileTransferState;
pub use operations::{
    verify_chunk, get_file_list, get_file_metadata, get_versioned_metadata, process_received_chunk, delete_file,
};

