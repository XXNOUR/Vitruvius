// src/watcher.rs
use std::path::PathBuf;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::gui::{GuiEvent, GuiFileInfo};
use crate::storage;

// What the watcher tells main happened — main owns the swarm
// so it's the only one that can send SyncMessages to peers
pub enum WatchNotification {
    FileChanged { file_name: String },
    FileDeleted { file_name: String },
}

pub async fn run_watcher(
    path: PathBuf,
    event_tx: mpsc::UnboundedSender<GuiEvent>,
    notify_tx: mpsc::UnboundedSender<WatchNotification>,
) {
    // bridge between notify's sync callback and our async loop
    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<notify::Event>();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = fs_tx.send(ev);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create watcher: {}", e);
                return;
            }
        };

    if let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive) {
        warn!("Failed to watch {:?}: {}", path, e);
        return;
    }

    info!("Watching folder: {:?}", path);

    // debounce ticker — only rescan after filesystem goes quiet for 500ms
    // prevents spamming list_folder while a large file is still being written
    let mut debounce = tokio::time::interval(Duration::from_millis(500));
    let mut dirty = false;

    loop {
        tokio::select! {
            Some(ev) = fs_rx.recv() => {
                match ev.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for changed_path in &ev.paths {
                            // only notify peers about actual files, not directories
                            if changed_path.is_file() {
                                if let Some(file_name) = changed_path
                                    .strip_prefix(&path)
                                    .ok()
                                    .and_then(|r| r.to_str())
                                    .map(|s| s.replace('\\', "/")) // normalize on Windows
                                {
                                    let _ = notify_tx.send(
                                        WatchNotification::FileChanged { file_name }
                                    );
                                }
                            }
                        }
                        dirty = true;
                    }

                    EventKind::Remove(_) => {
                        for removed_path in &ev.paths {
                            if let Some(file_name) = removed_path
                                .strip_prefix(&path)
                                .ok()
                                .and_then(|r| r.to_str())
                                .map(|s| s.replace('\\', "/"))
                            {
                                let _ = notify_tx.send(
                                    WatchNotification::FileDeleted { file_name }
                                );
                            }
                        }
                        dirty = true;
                    }

                    _ => {}
                }
            }

            _ = debounce.tick() => {
                if !dirty { continue; }
                dirty = false;

                // rescan and push updated listing to the GUI
                match storage::list_folder(&path).await {
                    Ok(files) => {
                        let listing = files
                            .iter()
                            .map(|f| GuiFileInfo {
                                name: f.file_name.clone(),
                                size: f.file_size,
                                chunks: f.total_chunks,
                            })
                            .collect();
                        let _ = event_tx.send(GuiEvent::FolderListing { files: listing });
                        info!("Folder changed — listing refreshed");
                    }
                    Err(e) => warn!("Watcher rescan failed: {}", e),
                }
            }
        }
    }
}
