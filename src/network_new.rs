use anyhow::Result;
use libp2p::{gossipsub, mdns, noise, swarm::NetworkBehaviour, tcp, yamux, Swarm, SwarmBuilder};
use std::time::Duration;

/// Network behaviour combining mDNS discovery and Gossipsub messaging
#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    pub mdns: mdns::tokio::Behaviour,
    pub gossipsub: gossipsub::Behaviour,
}

/// Create and configure the libp2p Swarm
pub async fn create_swarm() -> Result<Swarm<MyBehaviour>> {
    // Create Gossipsub config
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()
        .map_err(|e| anyhow::anyhow!("Gossipsub config error: {}", e))?;

    // Create Gossipsub behaviour
    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(libp2p::identity::Keypair::generate_ed25519()),
        gossipsub_config,
    )
    .map_err(|e| anyhow::anyhow!("Failed to create gossipup {}", e));

    // Subscribe to file transfer topic
    // Line removed

    // Build the swarm
    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            Ok(MyBehaviour {
                mdns: mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?,
                gossipsub: gossipsub.expect("REASON"),
            })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}
