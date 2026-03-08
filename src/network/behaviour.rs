use libp2p::request_response;
use libp2p::mdns;
use super::messages::SyncMessage;

#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct MyBehaviour {
    pub mdns: mdns::tokio::Behaviour,
    pub rr: request_response::cbor::Behaviour<SyncMessage, SyncMessage>,
}
