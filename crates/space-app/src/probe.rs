//! Synthetic work only; never enumerates the selected directory.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
pub const ROW_COUNT: usize = 100_000;
#[derive(Default)]
pub struct Progress {
    pub completed: AtomicUsize,
    pub cancelled: AtomicBool,
}
pub struct Probe {
    pub generation: u64,
    pub progress: Arc<Progress>,
}
impl Probe {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            progress: Arc::default(),
        }
    }
    pub fn cancel(&self) {
        self.progress.cancelled.store(true, Ordering::Release);
    }
    pub fn completed(&self) -> usize {
        self.progress.completed.load(Ordering::Acquire)
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub fn next_selection(current: usize, delta: isize, count: usize) -> usize {
    current
        .saturating_add_signed(delta)
        .min(count.saturating_sub(1))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_clamps() {
        assert_eq!(next_selection(0, -1, ROW_COUNT), 0);
        assert_eq!(next_selection(ROW_COUNT - 1, 1, ROW_COUNT), ROW_COUNT - 1);
        assert_eq!(next_selection(4, 1, ROW_COUNT), 5);
        assert_eq!(next_selection(0, 1, 0), 0);
    }
    #[test]
    fn dropping_probe_cancels_worker() {
        let probe = Probe::new(1);
        let progress = probe.progress.clone();
        drop(probe);
        assert!(progress.cancelled.load(Ordering::Acquire));
    }
    #[test]
    fn replacement_has_independent_progress() {
        let old = Probe::new(1);
        let new = Probe::new(2);
        old.progress.completed.store(99, Ordering::Release);
        old.cancel();
        assert_eq!(new.completed(), 0);
        assert!(!new.progress.cancelled.load(Ordering::Acquire));
        assert_ne!(old.generation, new.generation);
    }
}
