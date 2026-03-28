// src/main.rs
mod gui;
mod network;
mod state;
mod storage;
mod sync;
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
            _ => {}
        }
        i += 1;
    }

    let node_name = get_node_name();
    info!(
        "Node: {} | HTTP :{} | WS :{},|THEME : {}",
        node_name, http_port, ws_port, theme
    );

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<gui::GuiEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<gui::GuiCommand>();
    let state = Arc::new(Mutex::new(AppState::new(node_name.clone())));

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

    // peer_id → per-peer download state (active transfers + queue)
    let mut transfers: HashMap<libp2p::PeerId, PeerDownload> = HashMap::new();
    let mut cmd_rx: mpsc::UnboundedReceiver<gui::GuiCommand> = cmd_rx;
    // Stall checker: if a file hasn't received a chunk in 20s, re-request missing chunks
    let mut stall_tick = interval(Duration::from_secs(10));

    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<PathBuf>();
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<watcher::WatchNotification>();

    loop {
        tokio::select! {

                Some(cmd) = cmd_rx.recv() => {


                                sync::on_command(
                                    cmd, &mut swarm, Arc::clone(&state), &event_tx,&watch_tx, &node_name,
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
            tokio::spawn(watcher::run_watcher(path, event_tx2, notify_tx2));
        }

        Some(notification) = notify_rx.recv() => {
            match notification {
                watcher::WatchNotification::FileChanged { file_name } => {
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

                watcher::WatchNotification::FileDeleted { file_name } => {
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
                            _ = tokio::signal::ctrl_c() => {
                                info!("Shutting down…");
                                break;
                            }
                        }
    }
    Ok(())
}
