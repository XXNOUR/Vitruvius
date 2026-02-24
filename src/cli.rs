use crate::file_transfer::prepare_file_transfer;
use anyhow::Result;
use libp2p::{gossipsub, PeerId, Swarm};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::network_new::MyBehaviour;
/// Handle user commands
pub async fn handle_command(
    line: &str,
    swarm: &mut Swarm<MyBehaviour>,
    peers: &[PeerId],
    _sync_dir: &Path,
) -> Result<()> {
    let parts: Vec<&str> = line.split_whitespace().collect();

    match parts.first() {
        Some(&"send") => {
            if parts.len() < 2 {
                info!("Usage: send <filepath>");
                return Ok(());
            }

            let file_path = PathBuf::from(parts[1]);

            if !file_path.exists() {
                info!("File not found: {}", file_path.display());
                return Ok(());
            }

            if peers.is_empty() {
                info!("No peers discovered yet. Waiting for peers...");
                return Ok(());
            }

            info!(
                "Sending file '{}' to {} peer(s)",
                file_path.display(),
                peers.len()
            );

            // Prepare file transfer messages
            let messages = prepare_file_transfer(&file_path)?;

            // Get the gossipsub topic
            let topic = gossipsub::IdentTopic::new("vitruvius-file-transfer");

            // Send each message
            for msg in messages {
                let data = bincode::serialize(&msg)?;
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic.clone(), data)?;
            }

            info!(" File sent successfully!");
        }

        Some(&"peers") => {
            if peers.is_empty() {
                info!("No peers discovered");
            } else {
                info!("Discovered peers:");
                for peer in peers {
                    info!("  - {}", peer);
                }
            }
        }

        Some(&"quit") => {
            info!("Shutting down...");
            std::process::exit(0);
        }

        Some(&"help") => {
            info!("Commands:");
            info!("  send <filepath>  - Send a file to all discovered peers");
            info!("  peers            - List discovered peers");
            info!("  quit             - Exit");
        }

        _ => {
            info!("Unknown command. Type 'help' for available commands.");
        }
    }

    Ok(())
}
