// src/main.rs
mod gui;
mod network;
mod state;
mod storage;
mod sync;

use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{interval, Duration};
use tracing::info;

use state::{AppState, get_node_name};
use sync::PeerDownload;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let mut http_port: u16 = 9000;
    let mut ws_port:   u16 = 9001;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--http-port" => { i += 1; if let Some(v) = args.get(i) { http_port = v.parse().unwrap_or(9000); } }
            "--ws-port"   => { i += 1; if let Some(v) = args.get(i) { ws_port   = v.parse().unwrap_or(9001); } }
            _ => {}
        }
        i += 1;
    }

    let node_name = get_node_name();
    info!("Node: {} | HTTP :{} | WS :{}", node_name, http_port, ws_port);

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<gui::GuiEvent>();
    let (cmd_tx,       cmd_rx)   = mpsc::unbounded_channel::<gui::GuiCommand>();
    let state = Arc::new(Mutex::new(AppState::new(node_name.clone())));

    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<String>(512);
    let broadcast_tx = Arc::new(broadcast_tx);

    {
        let btx = Arc::clone(&broadcast_tx);
        tokio::spawn(async move {
            while let Some(evt) = event_rx.recv().await {
                if let Ok(json) = serde_json::to_string(&evt) { let _ = btx.send(json); }
            }
        });
    }

    {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", http_port)).await?;
        info!("GUI  →  http://127.0.0.1:{}", http_port);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(gui::http::serve(stream, ws_port));
            }
        });
    }

    let mut swarm: libp2p::Swarm<network::MyBehaviour> = network::setup_network().await?;
    let my_peer_id = swarm.local_peer_id().to_string();

    {
        let listener  = TcpListener::bind(format!("0.0.0.0:{}", ws_port)).await?;
        let cmd_tx2   = cmd_tx.clone();
        let btx2      = Arc::clone(&broadcast_tx);
        let my_id2    = my_peer_id.clone();
        let my_name2  = node_name.clone();
        let state2    = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, addr)) = listener.accept().await {
                info!("GUI connected from {}", addr);
                let cmd_tx  = cmd_tx2.clone();
                let mut brx = btx2.subscribe();
                let pid     = my_id2.clone();
                let name    = my_name2.clone();
                let st      = Arc::clone(&state2);
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

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                sync::on_command(
                    cmd, &mut swarm, Arc::clone(&state), &event_tx, &node_name,
                ).await;
            }
            event = swarm.select_next_some() => {
                sync::on_swarm_event(
                    event, &mut swarm, Arc::clone(&state), &event_tx, &mut transfers,
                ).await;
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
