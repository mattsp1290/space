use crate::disk_model::CardId;
use space_core::ScanId;
/// Identity of the immutable card publication that produced a UI callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub card: CardId,
    pub scan: Option<ScanId>,
    pub query_epoch: u64,
}
