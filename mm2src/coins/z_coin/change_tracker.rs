use mm2_number::BigDecimal;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Tracks unconfirmed change notes and their associated transaction IDs.
#[derive(Debug, Clone)]
pub struct ChangeNoteTracker {
    tracker: Arc<Mutex<HashMap<Vec<u8>, BigDecimal>>>,
}

impl ChangeNoteTracker {
    pub fn new() -> Self {
        ChangeNoteTracker {
            tracker: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Adds a change note to the tracker.
    pub fn add_change_note(&self, tx_hex: Vec<u8>, change_note: BigDecimal) {
        let mut tracker = self.tracker.lock().unwrap();
        tracker.entry(tx_hex).or_insert_with(|| change_note);
    }

    /// Removes a change note from the tracker when the transaction is confirmed.
    pub fn remove_change_notes(&self, tx_hex: &Vec<u8>) -> Option<BigDecimal> {
        let mut tracker = self.tracker.lock().unwrap();
        tracker.remove(tx_hex)
    }

    /// Retrieves all unconfirmed change notes for a given transaction ID.
    pub fn change_notes_iter(&self) -> Vec<(Vec<u8>, BigDecimal)> {
        self.tracker.lock().unwrap().clone().into_iter().collect()
    }

    /// Retrieves all unconfirmed change notes across all transactions.
    pub fn sum_all_change_notes(&self) -> BigDecimal {
        let tracker = self.tracker.lock().unwrap();
        tracker.values().sum()
    }
}
