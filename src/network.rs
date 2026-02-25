use anyhow::Result;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, mdns, noise, request_response,
    swarm::SwarmEvent, tcp, yamux,
};
use std::{collections::HashMap, path::PathBuf, time::Duration};

use dialoguer::Input;
use libp2p::futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SyncMessage {
    file_name: String,
    content: Option<Vec<u8>>,
}

#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct MyBehaviour {
    mdns: mdns::tokio::Behaviour,
    rr: request_response::cbor::Behaviour<SyncMessage, SyncMessage>,
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
            // Set a long timeout so it doesn't drop the connection while reading big files
            config.set_request_timeout(Duration::from_secs(30));

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

pub async fn handle_swarm_event(swarm: &mut Swarm<MyBehaviour>) -> Result<()> {
    let target_id_str: String = Input::new()
        .with_prompt("Enter Peer ID to sync with (or hit Enter to wait)")
        .allow_empty(true)
        .interact_text()?;

    let target_peer_id = target_id_str.parse::<PeerId>().ok();

    let sync_path_input: String = Input::new()
        .with_prompt("Enter folder path")
        .interact_text()?;
    let sync_path = PathBuf::from(&sync_path_input);
    if !sync_path.exists() {
        fs::create_dir_all(&sync_path)?;
    }
    let sync_path_abs = fs::canonicalize(&sync_path)?;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                for (peer_id, addr) in list {
                    if let Some(target) = target_peer_id {
                        if peer_id == target {
                            println!("Match Found! Found {} at {}. Dialing...", target, addr);
                            swarm.dial(addr)?;
                        }
                    }
                }
            }

            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                println!("Network Connection Established with {}", peer_id);
                // Peer 2 triggers the pull
                if let Some(target) = target_peer_id {
                    if peer_id == target {
                        println!("Initiating protocol request to target...");
                        swarm.behaviour_mut().rr.send_request(
                            &peer_id,
                            SyncMessage {
                                file_name: "MANIFEST_REQ".into(),
                                content: None,
                            },
                        );
                    }
                }
            }

            SwarmEvent::Behaviour(MyBehaviourEvent::Rr(request_response::Event::Message {
                peer,
                message,
            })) => match message {
                request_response::Message::Request { channel, .. } => {
                    println!("Incoming sync request from: {}", peer);

                    let mut response = SyncMessage {
                        file_name: "EMPTY".into(),
                        content: None,
                    };

                    if let Ok(entries) = fs::read_dir(&sync_path_abs) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_file() {
                                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                                if let Ok(data) = fs::read(&path) {
                                    println!("Reading file: {} ({} bytes)", name, data.len());
                                    response = SyncMessage {
                                        file_name: name,
                                        content: Some(data),
                                    };
                                    break;
                                }
                            }
                        }
                    }
                    let _ = swarm.behaviour_mut().rr.send_response(channel, response);
                }
                request_response::Message::Response { response, .. } => {
                    if let Some(data) = response.content {
                        if response.file_name != "EMPTY" {
                            let dest = sync_path_abs.join(&response.file_name);
                            fs::write(&dest, &data)?;
                            println!("SUCCESS: Sync Complete. Saved: {:?}", dest);
                        } else {
                            println!("Notice: Remote folder was empty.");
                        }
                    }
                }
                _ => {}
            },
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                println!("Connection failed to {:?}: {}", peer_id, error);
            }
            _ => {}
        }
    }
}
