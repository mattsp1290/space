//! UI-only single-flight path action state.

#[derive(Default)]
pub struct Intent {
    pub path_busy: bool,
}
impl Intent {
    pub fn complete_path(&mut self, still_current: bool) -> bool {
        self.path_busy = false;
        still_current
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_and_current_path_completions_release_single_flight() {
        for current in [true, false] {
            let mut intent = Intent { path_busy: true };
            assert_eq!(intent.complete_path(current), current);
            assert!(!intent.path_busy);
        }
    }
}
