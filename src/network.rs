use anyhow::Result;
use libp2p::{Multiaddr, PeerId, Swarm, SwarmBuilder, mdns, noise, swarm::SwarmEvent, tcp, yamux};
use std::{collections::HashMap, time::Duration};
use tracing::{info, warn};

async fn setup_network() -> Result<Swarm<mdns::tokio::Behaviour>> {
    let local_key = libp2p::identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());

    let mdns_config = mdns::Config::default();
    let mdns_behav = mdns::tokio::Behaviour::new(mdns_config, local_peer_id)?;

    let mut swarm = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|_| mdns_behav)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    Ok(swarm)
}

/// Handle swarm events (peer discovery, connections, incoming messages)
async fn handle_swarm_event(
    event: SwarmEvent<mdns::Event>,
    discoverd_peers: &mut HashMap<PeerId, Vec<Multiaddr>>,
) {
    match event {
        SwarmEvent::Behaviour(mdns::Event::Discovered(peers)) => {
            for (peer_id, addr) in peers {
                info!(" Discovered peer: {} at {}", peer_id, addr);

                discoverd_peers
                    .entry(peer_id)
                    .or_insert(Vec::new())
                    .push(addr);
            }
        }
        SwarmEvent::Behaviour(mdns::Event::Expired(peers)) => {
            for (peer_id, addr) in peers {
                warn!(" Peer expired: {} at {}", peer_id, addr);

                discoverd_peers.remove(&peer_id);
            }
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Listening on: {}", address);
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            info!(" Connected to peer: {}", peer_id);
        }
        SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
            warn!(" Connection closed with {}: {:?}", peer_id, cause);
        }
        _ => {}
    }
}
