//! Isolated real-scanner fixtures shared by geometry and UI journey tests.
use crate::backend;
use space_core::{Cancellation, ScanId};
use space_store::RootRecord;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Fixture {
    pub directory: PathBuf,
    pub cache_directory: PathBuf,
    pub backend: backend::Backend,
    pub roots: Vec<RootRecord>,
    pub scans: Vec<ScanId>,
}
impl Fixture {
    pub fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "space-redesign-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let cache_directory = directory.join("cache");
        let opened = backend::open(&cache_directory).unwrap();
        let mut roots = vec![];
        let mut scans = vec![];
        for (index, name) in ["alpha", "bravo"].into_iter().enumerate() {
            let root = directory.join(name);
            std::fs::create_dir_all(root.join(format!("{name}-folder"))).unwrap();
            for i in 0..105 {
                std::fs::write(root.join(format!("{name}-folder/{name}-{i:03}.bin")), [0]).unwrap();
            }
            std::fs::write(root.join(format!("{name}.bin")), vec![0; 1000]).unwrap();
            if index == 0 {
                for path in ["a", "a/b", "a/b/c"] {
                    std::fs::create_dir_all(root.join(path)).unwrap();
                    for i in 0..20 {
                        std::fs::write(
                            root.join(path).join(format!("data-{i:03}.bin")),
                            vec![0; 1000 + i * 100],
                        )
                        .unwrap();
                    }
                }
            }
            let done = backend::scan(
                &opened.backend,
                backend::ScanRequest {
                    path: root,
                    cache_directory: cache_directory.clone(),
                    expected_root: None,
                    refresh_root: None,
                    generation: index as u64 + 1,
                },
                &Cancellation::new(),
                &backend::Progress::default(),
            )
            .unwrap()
            .unwrap();
            roots = done.roots;
            scans.push(done.snapshot.scan);
        }
        Self {
            directory,
            cache_directory,
            backend: opened.backend,
            roots,
            scans,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
