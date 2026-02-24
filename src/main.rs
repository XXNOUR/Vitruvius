// src/main.rs
use anyhow::Result;
use libp2p::{
    futures::StreamExt,
    gossipsub, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, PeerId, Swarm, SwarmBuilder,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::{io, io::AsyncBufReadExt, select};
use tracing::{info, Level};
use tracing_subscriber;
mod cli;
mod file_transfer;
mod network_new;
use crate::file_transfer::{handle_file_message, prepare_file_transfer, FileMessage, ReceiveState};

/// Network behaviour combining mDNS discovery and Gossipsub messaging
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    pub mdns: mdns::tokio::Behaviour,
    pub gossipsub: gossipsub::Behaviour,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("Starting Vitruvius file sync...");

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let sync_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from("./sync")
    };

    info!("Sync directory: {}", sync_dir.display());
    std::fs::create_dir_all(&sync_dir)?;

    // Create a Swarm
    let mut swarm = network_new::create_swarm().await?;

    let topic = gossipsub::IdentTopic::new("Vitruvuis-file-transfer");

    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Listen on all interfaces
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Track receive states
    let mut receive_states: HashMap<String, ReceiveState> = HashMap::new();

    // Track discovered peers
    let mut peers: Vec<PeerId> = Vec::new();

    // Setup stdin for commands
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    info!("Ready! Commands:");
    info!("  send <filepath>  - Send a file to all discovered peers");
    info!("  peers            - List discovered peers");
    info!("  quit             - Exit");

    loop {
        select! {
            // Handle user input
            Ok(Some(line)) = stdin.next_line() => {

                cli::handle_command(
                    line.trim(),
                    &mut swarm,
                    &peers,
                    &sync_dir
                ).await?;
            }

            // Handle network events
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {}", address);
                    }

                    SwarmEvent::Behaviour(network_new::MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer_id, addr) in list {
                            info!("Discovered peer: {} at {}", peer_id, addr);
                            if !peers.contains(&peer_id) {
                                peers.push(peer_id);

                                // Subscribe peer to gossipsub topic
                                swarm.behaviour_mut()
                                    .gossipsub
                                    .add_explicit_peer(&peer_id);
                            }
                        }
                    }

                    SwarmEvent::Behaviour(network_new::MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                        for (peer_id, _) in list {
                            info!("Peer expired: {}", peer_id);
                            peers.retain(|p| p != &peer_id);

                            swarm.behaviour_mut()
                                .gossipsub
                                .remove_explicit_peer(&peer_id);
                        }
                    }

                    SwarmEvent::Behaviour(network_new::MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                        message,
                        ..
                    })) => {
                        // Deserialize the message
                        if let Ok(file_msg) = bincode::deserialize::<FileMessage>(&message.data) {
                            match handle_file_message(
                                file_msg,
                                &mut receive_states,
                                &sync_dir
                            ) {
                                Ok(Some(path)) => {
                                    info!(" File received successfully: {}", path.display());
                                }
                                Ok(None) => {
                                    // Still receiving chunks
                                }
                                Err(e) => {
                                    info!("Error handling file message: {}", e);
                                }
                            }
                        }
                    }

                    _ => {}
                }
            }
        }
    }
}
