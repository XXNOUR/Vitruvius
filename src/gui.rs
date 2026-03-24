use crate::backend::{UiCommand, UiEvent, run_backend};
use eframe::egui;
use std::time::Duration;
use tokio::sync::mpsc;

/// Main GUI application state.
pub struct VitruviusApp {
    cmd_tx: mpsc::Sender<UiCommand>,
    event_rx: mpsc::Receiver<UiEvent>,

    // UI state
    local_peer_id: String,
    target_peer_id_input: String,
    folder_path_input: String,
    is_syncing: bool,
    connected_peers: Vec<String>,
    active_transfers: Vec<(String, f32)>, // (filename, progress 0.0–100.0)
    completed_files: Vec<String>,
    log_messages: Vec<String>,
}

impl VitruviusApp {
    fn new(cmd_tx: mpsc::Sender<UiCommand>, event_rx: mpsc::Receiver<UiEvent>) -> Self {
        Self {
            cmd_tx,
            event_rx,
            local_peer_id: "Waiting...".into(),
            target_peer_id_input: String::new(),
            folder_path_input: String::new(),
            is_syncing: false,
            connected_peers: Vec::new(),
            active_transfers: Vec::new(),
            completed_files: Vec::new(),
            log_messages: Vec::new(),
        }
    }

    /// Drain all pending backend events (non-blocking).
    fn poll_events(&mut self) -> bool {
        let mut received_any = false;
        while let Ok(event) = self.event_rx.try_recv() {
            received_any = true;
            match event {
                UiEvent::LocalPeerId(id) => {
                    self.local_peer_id = id;
                }
                UiEvent::PeerDiscovered { peer_id, addr } => {
                    self.log_messages
                        .push(format!("Discovered peer: {} at {}", peer_id, addr));
                }
                UiEvent::PeerConnected(id) => {
                    if !self.connected_peers.contains(&id) {
                        self.connected_peers.push(id.clone());
                    }
                    self.log_messages
                        .push(format!("Connected to {}", id));
                }
                UiEvent::PeerDisconnected(id) => {
                    self.connected_peers.retain(|p| p != &id);
                    self.log_messages
                        .push(format!("Disconnected from {}", id));
                }
                UiEvent::SyncProgress { file_name, percent } => {
                    if let Some(entry) = self
                        .active_transfers
                        .iter_mut()
                        .find(|(name, _)| name == &file_name)
                    {
                        entry.1 = percent;
                    } else {
                        self.active_transfers.push((file_name, percent));
                    }
                }
                UiEvent::FileComplete(name) => {
                    self.active_transfers.retain(|(n, _)| n != &name);
                    self.completed_files.push(name.clone());
                    self.log_messages
                        .push(format!("File complete: {}", name));
                }
                UiEvent::SyncComplete => {
                    self.active_transfers.clear();
                    self.log_messages.push("Sync complete!".into());
                }
                UiEvent::Log(msg) => {
                    self.log_messages.push(msg);
                }
                UiEvent::Error(msg) => {
                    self.log_messages.push(format!("[ERROR] {}", msg));
                }
            }
        }
        received_any
    }
}

impl eframe::App for VitruviusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll backend events
        if self.poll_events() {
            ctx.request_repaint();
        }

        // Schedule periodic repaints to poll for new events
        ctx.request_repaint_after(Duration::from_millis(100));

        // --- Top bar ---
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Vitruvius");
                ui.separator();
                ui.label("Peer ID:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.local_peer_id.as_str())
                        .desired_width(400.0)
                        .interactive(false),
                );
                if ui.button("Copy").clicked() {
                    ctx.copy_text(self.local_peer_id.clone());
                }
            });
        });

        // --- Log panel (bottom) ---
        egui::TopBottomPanel::bottom("log_panel")
            .resizable(true)
            .min_height(120.0)
            .show(ctx, |ui| {
                ui.heading("Log");
                ui.separator();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for msg in &self.log_messages {
                            ui.label(msg);
                        }
                    });
            });

        // --- Left config panel ---
        egui::SidePanel::left("config_panel")
            .resizable(true)
            .min_width(280.0)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("Configuration");
                ui.separator();

                ui.add_space(8.0);
                ui.label("Target Peer ID:");
                ui.add_enabled(
                    !self.is_syncing,
                    egui::TextEdit::singleline(&mut self.target_peer_id_input)
                        .hint_text("Leave empty to wait for connections"),
                );

                ui.add_space(8.0);
                ui.label("Folder Path:");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        !self.is_syncing,
                        egui::TextEdit::singleline(&mut self.folder_path_input)
                            .hint_text("/path/to/sync/folder"),
                    );
                    if !self.is_syncing && ui.button("Browse...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.folder_path_input = path.display().to_string();
                        }
                    }
                });

                ui.add_space(12.0);
                if !self.is_syncing {
                    let can_start = !self.folder_path_input.trim().is_empty();
                    if ui
                        .add_enabled(can_start, egui::Button::new("Start Sync"))
                        .clicked()
                    {
                        let cmd = UiCommand::StartSync {
                            peer_id_str: self.target_peer_id_input.clone(),
                            folder_path: self.folder_path_input.clone(),
                        };
                        let _ = self.cmd_tx.try_send(cmd);
                        self.is_syncing = true;
                        self.log_messages.push("Sync started.".into());
                    }
                } else if ui.button("Stop").clicked() {
                    let _ = self.cmd_tx.try_send(UiCommand::Stop);
                    self.is_syncing = false;
                    self.log_messages.push("Sync stopped.".into());
                }
            });

        // --- Central status panel ---
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Status");
            ui.separator();

            // Connected peers
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Connected Peers").strong(),
            );
            if self.connected_peers.is_empty() {
                ui.label("No peers connected.");
            } else {
                for peer in &self.connected_peers {
                    ui.horizontal(|ui| {
                        ui.label(format!("  {}", peer));
                    });
                }
            }

            ui.add_space(12.0);
            ui.separator();

            // Active transfers
            ui.label(
                egui::RichText::new("Active Transfers").strong(),
            );
            if self.active_transfers.is_empty() {
                ui.label("No active transfers.");
            } else {
                for (name, percent) in &self.active_transfers {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        ui.add(
                            egui::ProgressBar::new(*percent / 100.0)
                                .show_percentage()
                                .desired_width(200.0),
                        );
                    });
                }
            }

            ui.add_space(12.0);
            ui.separator();

            // Completed files
            ui.label(
                egui::RichText::new("Completed Files").strong(),
            );
            if self.completed_files.is_empty() {
                ui.label("No completed files yet.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for name in &self.completed_files {
                            ui.label(format!("  {}", name));
                        }
                    });
            }
        });
    }
}

/// Launch the eframe GUI (must be called from the main thread).
pub fn run_gui() {
    let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>(100);
    let (event_tx, event_rx) = mpsc::channel::<UiEvent>(100);

    // Spawn the tokio backend on a separate thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        if let Err(e) = rt.block_on(run_backend(cmd_rx, event_tx)) {
            eprintln!("Backend error: {}", e);
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "Vitruvius",
        options,
        Box::new(move |_cc| Ok(Box::new(VitruviusApp::new(cmd_tx, event_rx)))),
    ) {
        eprintln!("eframe error: {}", e);
    }
}
