// src/network.rs
use anyhow::Result;
use libp2p::{PeerId, StreamProtocol, Swarm, mdns, request_response};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SyncMessage {
    MetaDataRequest {
        file_name: String,
    },

    ListFiles,

    FileList {
        file_names: Vec<String>,
    },

    Metadata {
        file_name: String,
        total_chunks: usize,
        file_size: u64,
        chunk_hashes: Vec<[u8; 32]>,
    },

    ChunkRequest {
        file_name: String,
        chunk_index: usize,
    },

    ChunkResponse {
        chunk_index: usize,
        data: Vec<u8>,
        hash: [u8; 32],
        file_name: String,
    },

    FileChanged {
        file_name: String,
    },

    FileDeleted {
        file_name: String,
    },

    TransferComplete,

    Empty,

    Error {
        message: String,
    },
}

/// Manages concurrent file transfers with proper tracking
pub struct SyncSession {
    pub pending_files: Vec<String>,
    pub active_transfers: HashSet<String>,
    pub completed_files: HashSet<String>,
    pub max_concurrent: usize,
}

impl SyncSession {
    pub fn new(file_list: Vec<String>, max: usize) -> Self {
        SyncSession {
            pending_files: file_list,
            active_transfers: HashSet::new(),
            completed_files: HashSet::new(),
            max_concurrent: max,
        }
    }

    /// Check if we can start more file transfers
    pub fn can_start_more(&self) -> bool {
        self.active_transfers.len() < self.max_concurrent && !self.pending_files.is_empty()
    }

    /// Start the next file transfer, properly tracking it as active
    pub fn start_next(&mut self) -> Option<String> {
        if !self.can_start_more() {
            return None;
        }

        if let Some(file_name) = self.pending_files.pop() {
            self.active_transfers.insert(file_name.clone());
            Some(file_name)
        } else {
            None
        }
    }

    /// Mark a file transfer as complete
    pub fn mark_complete(&mut self, file_name: &str) {
        self.active_transfers.remove(file_name);
        self.completed_files.insert(file_name.to_string());
    }

    /// Check if all transfers are done
    pub fn is_done(&self) -> bool {
        self.pending_files.is_empty() && self.active_transfers.is_empty()
    }

    /// Get count of remaining files
    pub fn remaining(&self) -> usize {
        self.pending_files.len() + self.active_transfers.len()
    }

    /// Get count of completed files
    pub fn completed(&self) -> usize {
        self.completed_files.len()
    }
}

#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct MyBehaviour {
    pub mdns: mdns::tokio::Behaviour,
    pub rr: request_response::cbor::Behaviour<SyncMessage, SyncMessage>,
}

pub async fn setup_network() -> Result<Swarm<MyBehaviour>> {
    let local_key = libp2p::identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    println!("--- Vitruvius Node ---");
    println!("YOUR ID: {}", local_peer_id);

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            let mut config = request_response::Config::default();

            // Increase timeout for larger file transfers
            #[allow(deprecated)]
            config.set_request_timeout(Duration::from_secs(60));

            let rr = libp2p::request_response::cbor::Behaviour::<SyncMessage, SyncMessage>::new(
                [(
                    StreamProtocol::new("/vitruvius/sync/1.0"),
                    request_response::ProtocolSupport::Full,
                )],
                config,
            );
            Ok(MyBehaviour { mdns, rr })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    Ok(swarm)
}
