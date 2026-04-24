// src/main.rs
mod crypto;
mod gui;
mod identity;
mod network;
mod state;
mod storage;
mod sync;
mod tofu;
mod watcher;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use state::{get_node_name, AppState};
use sync::PeerDownload;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Duration};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let mut http_port: u16 = 9000;
    let mut ws_port: u16 = 9001;
    let mut theme = String::default();
    let mut key_path: Option<PathBuf> = None;
    let mut generate_key_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--http-port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    http_port = v.parse().unwrap_or(9000);
                }
            }
            "--ws-port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    ws_port = v.parse().unwrap_or(9001);
                }
            }
            "--theme" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    theme = v.clone();
                }
            }
            // --key-path <file>
            // Load a 32-byte key file. All peers in the sync group must use the same file.
            "--key-path" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    key_path = Some(PathBuf::from(v));
                }
            }
            // --generate-key <file>
            // Write a new random 32-byte key to <file> and exit.
            // Run once, copy the file to all peers, then start each with --key-path.
            "--generate-key" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    generate_key_path = Some(PathBuf::from(v));
                }
            }
            "--help" | "-h" => {
                println!("Vitruvius — zero-knowledge P2P file sync");
                println!();
                println!("USAGE:");
                println!("  vitruvius [OPTIONS]");
                println!();
                println!("OPTIONS:");
                println!("  --http-port <port>      GUI HTTP port (default: 9000)");
                println!("  --ws-port   <port>      GUI WebSocket port (default: 9001)");
                println!("  --theme     <name>      GUI theme (default: vitruvius)");
                println!(
                    "  --key-path  <file>      32-byte encryption key file (enables encryption)"
                );
                println!("  --generate-key <file>   Generate a new key file and exit");
                println!();
                println!("QUICK START (encrypted sync):");
                println!("  # Step 1 — generate a key (run once on any machine)");
                println!("  vitruvius --generate-key vitruvius.key");
                println!();
                println!("  # Step 2 — copy vitruvius.key to all peer machines");
                println!();
                println!("  # Step 3 — start Vitruvius on each peer with the key");
                println!("  vitruvius --key-path vitruvius.key");
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    // ── --generate-key: write key file and exit ───────────────────────────────
    if let Some(ref path) = generate_key_path {
        crypto::generate_key(path)?;
        return Ok(());
    }

    // ── Load encryption key (optional) ───────────────────────────────────────
    let encryption_key: Option<[u8; 32]> = match key_path {
        Some(ref path) => match crypto::load_key(path) {
            Ok(key) => {
                println!("Encryption enabled — key loaded from {}", path.display());
                Some(key)
            }
            Err(e) => {
                eprintln!("ERROR: Cannot load key file: {e}");
                eprintln!("       Generate one with: vitruvius --generate-key vitruvius.key");
                std::process::exit(1);
            }
        },
        None => None,
    };

    let node_name = get_node_name();
    info!(
        "Node: {} | HTTP :{} | WS :{} | THEME: {} | ENCRYPTED: {}",
        node_name,
        http_port,
        ws_port,
        theme,
        encryption_key.is_some()
    );

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<gui::GuiEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<gui::GuiCommand>();

    // Build AppState and store the encryption key inside it.
    // From this point on, all code that needs the key reads it from state.
    let mut initial_state = AppState::new(node_name.clone());
    initial_state.encryption_key = encryption_key;
    let state = Arc::new(Mutex::new(initial_state));

    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<String>(512);
    let broadcast_tx = Arc::new(broadcast_tx);

    {
        let btx = Arc::clone(&broadcast_tx);
        tokio::spawn(async move {
            while let Some(evt) = event_rx.recv().await {
                if let Ok(json) = serde_json::to_string(&evt) {
                    let _ = btx.send(json);
                }
            }
        });
    }

    {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", http_port)).await?;
        info!("GUI  →  http://127.0.0.1:{}", http_port);
        let theme = theme.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let themee = theme.clone();
                tokio::spawn(gui::http::serve(stream, ws_port, themee));
            }
        });
    }

    let mut swarm: libp2p::Swarm<network::MyBehaviour> = network::setup_network().await?;
    let my_peer_id = swarm.local_peer_id().to_string();

    {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", ws_port)).await?;
        let cmd_tx2 = cmd_tx.clone();
        let btx2 = Arc::clone(&broadcast_tx);
        let my_id2 = my_peer_id.clone();
        let my_name2 = node_name.clone();
        let state2 = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, addr)) = listener.accept().await {
                info!("GUI connected from {}", addr);
                let cmd_tx = cmd_tx2.clone();
                let mut brx = btx2.subscribe();
                let pid = my_id2.clone();
                let name = my_name2.clone();
                let st = Arc::clone(&state2);
                tokio::spawn(async move {
                    gui::ws::handle_client(stream, cmd_tx, &mut brx, pid, name, st).await;
                });
            }
        });
    }

    let mut transfers: HashMap<libp2p::PeerId, PeerDownload> = HashMap::new();
    let mut cmd_rx: mpsc::UnboundedReceiver<gui::GuiCommand> = cmd_rx;
    let mut stall_tick = interval(Duration::from_secs(10));

    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<PathBuf>();
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<watcher::WatchNotification>();

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                sync::on_command(
                    cmd, &mut swarm, Arc::clone(&state), &event_tx, &watch_tx, &node_name,
                ).await;
            }

            event = swarm.select_next_some() => {
                sync::on_swarm_event(
                    event, &mut swarm, Arc::clone(&state), &event_tx, &mut transfers,
                ).await;
            }

            Some(path) = watch_rx.recv() => {
                let event_tx2 = event_tx.clone();
                let notify_tx2 = notify_tx.clone();
                let state3 = Arc::clone(&state);
                tokio::spawn(watcher::run_watcher(path, event_tx2, notify_tx2, state3));
            }

            Some(notification) = notify_rx.recv() => {
                match notification {
                    watcher::WatchNotification::FileChanged { file_name } => {
                        let should_send = {
                            let mut st = state.lock().await;
                            let now = std::time::Instant::now();
                            let recent = st.recently_notified.get(&file_name)
                                .map(|t| t.elapsed().as_secs() < 5)
                                .unwrap_or(false);
                            if !recent {
                                st.recently_notified.insert(file_name.clone(), now);
                                true
                            } else {
                                false
                            }
                        };

                        if should_send {
                            let peers: Vec<_> = state.lock().await
                                .connected_peers.iter().cloned().collect();
                            for peer in peers {
                                swarm.behaviour_mut().rr.send_request(
                                    &peer,
                                    network::SyncMessage::FileChanged {
                                        file_name: file_name.clone(),
                                    },
                                );
                            }
                            info!("Notified peers: {} changed", file_name);
                        }
                    }

                    watcher::WatchNotification::FileDeleted { file_name } => {
                        state.lock().await.recently_notified.remove(&file_name);

                        let peers: Vec<_> = state.lock().await
                            .connected_peers.iter().cloned().collect();
                        for peer in peers {
                            swarm.behaviour_mut().rr.send_request(
                                &peer,
                                network::SyncMessage::FileDeleted {
                                    file_name: file_name.clone(),
                                },
                            );
                        }
                        info!("Notified peers: {} deleted", file_name);
                    }
                }
            }

            _ = stall_tick.tick() => {
                sync::check_stalls(&mut swarm, &event_tx, &mut transfers).await;
            }
        }
    }
}
