//! Deterministic native metadata fixture; creates only its own temporary tree.
use space_core::*;
use space_scan::{ScanLimits, inspect_root, scan};
use std::{
    fs, io,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: u64 = std::env::args()
        .nth(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(10_000);
    if !(1..=1_000_000).contains(&count) {
        return Err("fixture count must be 1..=1000000".into());
    }
    let token = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!(
        "space-native-fixture-{}-{token}",
        std::process::id()
    ));
    fs::create_dir(&path)?;
    let fixture = Fixture(fs::canonicalize(path)?);
    for bucket in 0..100 {
        fs::create_dir(fixture.0.join(format!("bucket-{bucket:02}")))?;
    }
    // Lengths, layout, and names are deterministic. Files are sparse to avoid
    // writing content; physical allocation is measured independently.
    for index in 0..count {
        fs::File::create(
            fixture
                .0
                .join(format!("bucket-{:02}/entry-{index:08}", index % 100)),
        )?
        .set_len((index * 7919) % 1_048_576)?;
    }
    let spec = ScanSpec {
        key: ScanKey {
            id: ScanId::new(1)?,
            generation: 1,
        },
        root_id: RootId::new(1)?,
        lineage_id: LineageId::new(1)?,
        expected_root: inspect_root(&fixture.0)?,
        policy: ScanPolicy::default(),
    };
    for pass in 1..=2 {
        let start = Instant::now();
        let mut entries = 0u64;
        let mut issues = 0u64;
        let mut logical = 0u64;
        let finish = scan(
            &fixture.0,
            &spec,
            &Cancellation::new(),
            ScanLimits::default(),
            |batch| {
                for record in batch.records {
                    match record {
                        ScanRecord::Entry(entry) => {
                            entries += 1;
                            logical = logical
                                .checked_add(entry.logical_bytes.unwrap_or(0))
                                .ok_or_else(|| io::Error::other("fixture total overflow"))?;
                        }
                        ScanRecord::Issue(_) => issues += 1,
                        _ => {}
                    }
                }
                Ok(())
            },
        )?;
        println!(
            "os={} arch={} fixture=v1 seed=7919 files={} entries={} issues={} logical_bytes={} pass={} cache_state={} elapsed_ms={} outcome={:?} peak_rss=not_measured",
            std::env::consts::OS,
            std::env::consts::ARCH,
            count,
            entries,
            issues,
            logical,
            pass,
            if pass == 1 {
                "uncontrolled_after_creation"
            } else {
                "warm"
            },
            start.elapsed().as_millis(),
            finish.outcome
        );
        if finish.outcome != TraversalOutcome::Finished || issues != 0 || entries != count + 101 {
            return Err("native fixture was incomplete".into());
        }
    }
    Ok(())
}
