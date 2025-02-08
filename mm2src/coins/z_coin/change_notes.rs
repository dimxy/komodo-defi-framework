use mm2_number::BigDecimal;
use std::{collections::{hash_map::IntoIter, HashMap},
          sync::Arc};
use tokio::sync::Mutex;

type ChangeNoteMap = HashMap<Vec<u8>, BigDecimal>;
pub struct ChangeNotes(Arc<Mutex<ChangeNoteMap>>);

impl ChangeNotes {
    pub fn init() -> Self { Self(Default::default()) }
    pub async fn remove_note(&self, k: &Vec<u8>) -> Option<BigDecimal> { self.0.lock().await.remove(k) }
    pub async fn get_note(&self, k: &Vec<u8>) -> Option<BigDecimal> { self.0.lock().await.get(k).cloned() }
    pub async fn notes(&self) -> IntoIter<Vec<u8>, BigDecimal> { self.0.lock().await.clone().into_iter() }
    pub async fn sum_change(&self) -> BigDecimal { self.notes().await.map(|(_, change)| change).sum::<BigDecimal>() }
}
