use anyhow::Result;
use libp2p::{PeerId, StreamProtocol, Swarm, mdns, request_response};
use std::time::Duration;
use super::behaviour::MyBehaviour;
use super::messages::SyncMessage;

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

            // Timeout for large file transfers (30 minutes for very large files)
            #[allow(deprecated)]
            config.set_request_timeout(Duration::from_secs(1800));

            let rr = libp2p::request_response::cbor::Behaviour::<SyncMessage, SyncMessage>::new(
                [(
                    StreamProtocol::new("/vitruvius/sync/1.0"),
                    request_response::ProtocolSupport::Full,
                )],
                config,
            );
            Ok(MyBehaviour { mdns, rr })
        })?
        // CRITICAL: Keep connections alive indefinitely for real-time sync
        // File changes can happen at any time, so we need persistent connections
        // This allows immediate file change notifications without reconnect overhead
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(u64::MAX)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    Ok(swarm)
}
