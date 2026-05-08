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
    let mut vault_key_path: Option<PathBuf> = None;
    let mut generate_key_path: Option<PathBuf> = None;
    // Vault mode (at-rest *.vit encryption) is OFF by default.
    // The zero-knowledge guarantee is on the WIRE — chunks and metadata are
    // fully encrypted in transit regardless of this flag. Vault mode is an
    // opt-in layer for users who also want disk encryption. Default OFF keeps
    // received files immediately usable.
    let mut vault_mode: bool = false;
    let mut encrypted_protocol: bool = true;
    let mut import_args: Option<(PathBuf, PathBuf)> = None; // (src, dst)
    let mut export_args: Option<(PathBuf, PathBuf)> = None; // (src.vit, dst)

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
            "--key-path" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    key_path = Some(PathBuf::from(v));
                }
            }
            "--generate-key" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    generate_key_path = Some(PathBuf::from(v));
                }
            }
            "--vault" => {
                vault_mode = true;
            }
            "--no-vault" => {
                vault_mode = false;
            }
            "--no-encrypted-protocol" => {
                encrypted_protocol = false;
            }
            "--vault-key" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    vault_key_path = Some(PathBuf::from(v));
                }
            }
            "import" => {
                let src = args.get(i + 1).cloned();
                let dst = args.get(i + 2).cloned();
                i += 2;
                if let (Some(s), Some(d)) = (src, dst) {
                    import_args = Some((PathBuf::from(s), PathBuf::from(d)));
                } else {
                    eprintln!("usage: vitruvius import <plaintext_dir> <vault_dir>");
                    std::process::exit(2);
                }
            }
            "export" => {
                let src = args.get(i + 1).cloned();
                let dst = args.get(i + 2).cloned();
                i += 2;
                if let (Some(s), Some(d)) = (src, dst) {
                    export_args = Some((PathBuf::from(s), PathBuf::from(d)));
                } else {
                    eprintln!("usage: vitruvius export <file.vit> <plaintext_dest>");
                    std::process::exit(2);
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    // ── --generate-key ────────────────────────────────────────────────────────
    if let Some(ref path) = generate_key_path {
        crypto::generate_key(path)?;
        return Ok(());
    }

    // ── Load operator-shared transport key (--key-path) ───────────────────────
    // ── Transport key: explicit --key-path skips TOFU; otherwise TOFU handles it
    let shared_key_from_cli = key_path.is_some();
    let encryption_key: Option<[u8; 32]> = match key_path {
        Some(ref path) => match crypto::load_key(path) {
            Ok(key) => {
                println!("Transport key loaded from {}", path.display());
                Some(key)
            }
            Err(e) => {
                eprintln!("ERROR: Cannot load key file: {e}");
                eprintln!("       Generate one with: vitruvius --generate-key vitruvius.key");
                std::process::exit(1);
            }
        },
        None => None, // TOFU will establish per-peer keys automatically
    };

    // ── Vault (at-rest) key — only needed when vault_mode OR import/export ─────
    // We never silently generate a vault key during a normal daemon run when
    // vault_mode is off — that would surprise the user with files in $HOME.
    let need_vault_key = vault_mode || import_args.is_some() || export_args.is_some();
    let vault_key: Option<[u8; 32]> = if need_vault_key {
        let path = vault_key_path.unwrap_or_else(default_vault_key_path);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            crypto::generate_key(&path)?;
            println!("Vault key auto-generated at {}", path.display());
        }
        Some(crypto::load_key(&path)?)
    } else {
        None
    };

    // ── Subcommand: bulk-import plaintext folder into a vault folder ─────────
    if let Some((src, dst)) = import_args {
        let key = vault_key.expect("vault_key required for import");
        let n = storage::import_plaintext_dir_into_vault(&src, &dst, &key)?;
        println!(
            "Imported {n} file(s) from {} into vault {}",
            src.display(),
            dst.display()
        );
        return Ok(());
    }
    if let Some((src, dst)) = export_args {
        let key = vault_key.expect("vault_key required for export");
        let bytes = storage::vault_export_to_plaintext(&src, &key, &dst)?;
        println!("Exported {bytes} bytes to {}", dst.display());
        return Ok(());
    }

    let node_name = get_node_name();
    info!(
        "Node: {} | HTTP :{} | WS :{} | THEME: {} | TRANSPORT_KEY: {} | VAULT: {} | ENCRYPTED_PROTO: {}",
        node_name,
        http_port,
        ws_port,
        theme,
        encryption_key.is_some(),
        vault_mode,
        encrypted_protocol,
    );

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<gui::GuiEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<gui::GuiCommand>();

    let mut initial_state = AppState::new(node_name.clone());
    initial_state.encryption_key = encryption_key;
    initial_state.shared_key_from_cli = shared_key_from_cli;
    initial_state.vault_key = vault_key;
    initial_state.vault_mode = vault_mode;
    initial_state.encrypted_protocol = encrypted_protocol;
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

fn default_vault_key_path() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".vitruvius");
    p.push("vault.key");
    p
}

fn print_help() {
    println!("Vitruvius — zero-knowledge P2P file sync");
    println!();
    println!("HOW ZERO-KNOWLEDGE WORKS:");
    println!("  The network (your ISP, routers, relays) sees NOTHING about what you sync:");
    println!("    · Filenames are opaque 16-byte IDs derived from your transport key.");
    println!("    · Content hashes are blinded — a relay cannot recognise a known file.");
    println!("    · The manifest (file list + sizes) is fully AEAD-encrypted.");
    println!("    · Every chunk is ChaCha20-Poly1305 encrypted with position-bound AAD.");
    println!("  Received files land as normal, readable files in your sync folder.");
    println!("  Zero-knowledge is about the wire — YOU can always read your own files.");
    println!();
    println!("USAGE:");
    println!("  vitruvius [OPTIONS]            # start the daemon (wire-ZK on by default)");
    println!("  vitruvius --generate-key <f>  # generate a 32-byte pre-shared key file");
    println!("  vitruvius import <src> <dst>  # bulk-encrypt a folder into *.vit blobs");
    println!("  vitruvius export <f.vit> <d>  # decrypt a *.vit blob to plaintext");
    println!();
    println!("DAEMON OPTIONS:");
    println!("  --http-port <port>            GUI HTTP port (default 9000)");
    println!("  --ws-port   <port>            GUI WebSocket port (default 9001)");
    println!("  --key-path  <file>            Pre-shared 32-byte transport key (skips TOFU)");
    println!("  --no-encrypted-protocol       Debug only: disable encrypted wire protocol");
    println!();
    println!("OPTIONAL AT-REST VAULT (disk encryption — separate from wire ZK):");
    println!("  --vault                       Store files as *.vit encrypted blobs on disk");
    println!("  --vault-key <file>            Path to vault key (auto-generated if missing)");
    println!("  --no-vault                    Explicitly disable vault (default)");
    println!();
    println!("QUICK START:");
    println!("  # Peer A and B on same LAN — just start and approve the TOFU popup:");
    println!("  vitruvius");
    println!("  # Pre-shared key mode (no TOFU needed):");
    println!("  vitruvius --generate-key sync.key");
    println!("  vitruvius --key-path sync.key   # on both peers");
}
