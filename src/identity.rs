// src/identity.rs
//
// Persistent node identity for Vitruvius.
//
// WHY THIS MATTERS:
//   libp2p assigns every node a PeerId derived from its Ed25519 keypair.
//   If we call Keypair::generate_ed25519() on every startup the node gets a
//   *different* PeerId each time.  This breaks TOFU completely:
//     - The TOFU store keys trusted peers by PeerId.
//     - A new PeerId means no stored key is ever found.
//     - The approval dialog fires on every reconnect instead of just once.
//
// THE FIX:
//   Save the keypair to disk the first time we start, then reload it on every
//   subsequent start.  The PeerId is then stable across restarts and TOFU
//   approval persists as intended.
//
// STORAGE:
//   The keypair is serialised to protobuf and written to `vitruvius_peer.key`
//   in the current working directory.  Losing this file is safe — a new
//   identity is generated automatically — but all peers that previously
//   trusted this node will ask for re-approval once, because from their
//   perspective this is a new peer.

use anyhow::Result;
use libp2p::identity::Keypair;
use std::fs;

const IDENTITY_FILE: &str = "vitruvius_peer.key";

/// Load the node's persistent Ed25519 keypair from `vitruvius_peer.key`.
/// If the file does not exist (first run) a new keypair is generated, saved,
/// and returned.  If the file is corrupted a new keypair replaces it.
///
/// The returned keypair should be passed directly to libp2p's SwarmBuilder so
/// that the PeerId is derived from it and remains constant across restarts.
pub fn load_or_create_keypair() -> Result<Keypair> {
    if let Ok(bytes) = fs::read(IDENTITY_FILE) {
        match Keypair::from_protobuf_encoding(&bytes) {
            Ok(kp) => {
                tracing::info!("Loaded persistent identity from {IDENTITY_FILE}");
                return Ok(kp);
            }
            Err(e) => {
                tracing::warn!(
                    "Identity file {IDENTITY_FILE} is corrupted ({e}) — regenerating"
                );
            }
        }
    }

    let kp = Keypair::generate_ed25519();
    let bytes = kp
        .to_protobuf_encoding()
        .map_err(|e| anyhow::anyhow!("Cannot serialise keypair: {e}"))?;
    fs::write(IDENTITY_FILE, &bytes)?;
    tracing::info!("Generated new persistent identity → saved to {IDENTITY_FILE}");
    Ok(kp)
}

