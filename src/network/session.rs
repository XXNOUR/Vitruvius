use std::collections::HashSet;

pub struct SyncSession {
    pub pending_files: Vec<String>,
    pub active_transfers: HashSet<String>,
    pub completed_files: HashSet<String>,
    pub max_concurrent: usize,
}

impl SyncSession {
    pub fn new(file_list: Vec<String>, max: usize) -> Self {
        SyncSession {
            pending_files: file_list,
            active_transfers: HashSet::new(),
            completed_files: HashSet::new(),
            max_concurrent: max,
        }
    }

    /// Check if we can start more file transfers
    pub fn can_start_more(&self) -> bool {
        self.active_transfers.len() < self.max_concurrent && !self.pending_files.is_empty()
    }

    pub fn start_next(&mut self) -> Option<String> {
        if !self.can_start_more() {
            return None;
        }

        if let Some(file_name) = self.pending_files.pop() {
            self.active_transfers.insert(file_name.clone());
            Some(file_name)
        } else {
            None
        }
    }

    pub fn mark_complete(&mut self, file_name: &str) {
        self.active_transfers.remove(file_name);
        self.completed_files.insert(file_name.to_string());
    }

    pub fn is_done(&self) -> bool {
        self.pending_files.is_empty() && self.active_transfers.is_empty()
    }

    pub fn remaining(&self) -> usize {
        self.pending_files.len() + self.active_transfers.len()
    }

    pub fn completed(&self) -> usize {
        self.completed_files.len()
    }
}
