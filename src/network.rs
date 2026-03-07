// src/network.rs
use anyhow::Result;
use libp2p::{PeerId, StreamProtocol, Swarm, mdns, request_response};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::storage::FileTransferState;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SyncMessage {
    Request {
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

    TransferComplete,

    Empty,

    Error {
        message: String,
    },
}
pub struct SyncSession {
 pub   pending_file: Vec<String>,
  pub   active_transfers: HashMap<String, FileTransferState>,
   pub  completed_files: HashSet<String>,
   pub  max_conc: usize,
}

impl SyncSession {
    pub fn new(file_list: Vec<String>) -> Self {
        SyncSession {
            pending_file: file_list,
            active_transfers: HashMap::new(),
            completed_files: HashSet::new(),
            max_conc: 3,
        }
    }

    pub fn can_start_more(&self) -> bool {
        self.active_transfers.len() < self.max_conc && !self.pending_file.is_empty()
    }

    pub fn start_next(&mut self) -> Option<String> {
        if self.can_start_more() {
            self.pending_file.pop()
        } else {
            None
        }
    }
    pub fn mark_complete(&mut self, file_name: &str) {
        self.active_transfers.remove(file_name);
        self.completed_files.insert(file_name.to_string());
    }
    pub fn is_done(&self) -> bool {
        self.pending_file.is_empty() && self.active_transfers.is_empty()
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

            #[allow(deprecated)]
            config.set_request_timeout(Duration::from_secs(10));

            let rr = libp2p::request_response::cbor::Behaviour::<SyncMessage, SyncMessage>::new(
                [(
                    StreamProtocol::new("/vitruvius/sync/1.0"),
                    request_response::ProtocolSupport::Full,
                )],
                config,
            );
            Ok(MyBehaviour { mdns, rr })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    Ok(swarm)
}
