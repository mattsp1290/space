//! UI-only intent state: async completion never consumes a newer folder choice.
use std::path::PathBuf;

#[derive(Default)]
pub struct Intent {
    pub chosen: Option<PathBuf>,
    pub path_busy: bool,
}
impl Intent {
    pub fn consume_choice(&mut self) {
        self.chosen = None;
    }
    pub fn complete_path(&mut self, still_current: bool) -> bool {
        self.path_busy = false;
        still_current
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newer_choice_survives_scan_completion() {
        let mut intent = Intent {
            chosen: Some("a".into()),
            ..Intent::default()
        };
        intent.consume_choice(); // scan A starts; snapshot completion does not consume choices
        intent.chosen = Some("b".into());
        assert_eq!(intent.chosen.as_deref(), Some(std::path::Path::new("b")));
    }
    #[test]
    fn stale_and_current_path_completions_release_single_flight() {
        for current in [true, false] {
            let mut intent = Intent {
                path_busy: true,
                ..Intent::default()
            };
            assert_eq!(intent.complete_path(current), current);
            assert!(!intent.path_busy);
        }
    }
}
