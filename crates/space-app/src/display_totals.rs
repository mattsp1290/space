//! Metric-independent cached display values, retained across metric switches.
use space_core::Metric;
use space_store::StoredEntry;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayTotals {
    pub logical: u128,
    pub allocated: u128,
}
impl DisplayTotals {
    pub fn from_entry(entry: &StoredEntry) -> Self {
        Self {
            logical: u128::from(entry.logical.observed_bytes),
            allocated: u128::from(entry.allocated.observed_bytes),
        }
    }
    pub fn bytes(self, metric: Metric) -> u128 {
        match metric {
            Metric::Logical => self.logical,
            Metric::Allocated => self.allocated,
        }
    }
}
