// The sync module handles synchronization coordination
// Currently synchronization logic is in app.rs, 
// but this module is ready for future sync-specific functionality

pub mod state;

pub use state::SyncState;
