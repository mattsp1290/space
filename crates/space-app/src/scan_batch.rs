//! Pure serial scan coordinator. Effects are performed by the app's background
//! workers; callbacks carry all three identities and cannot navigate the UI.
use crate::disk_model::{CardId, ScanTarget, same_root};
use space_core::ScanId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanToken {
    pub batch: BatchId,
    pub card_id: CardId,
    pub request: RequestId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchPhase {
    #[default]
    Idle,
    Discovering,
    Running,
    Stopping,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardStatus {
    Queued { ordinal: usize, total: usize },
    Scanning { ordinal: usize, total: usize },
    Stopping,
    Ready { scan: ScanId, partial: bool },
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanOutcome {
    Published { scan: ScanId, partial: bool },
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct ActiveScan {
    pub token: ScanToken,
    pub target: ScanTarget,
    pub ordinal: usize,
    pub total: usize,
}

#[derive(Clone, Debug)]
pub enum Event {
    ScanAll,
    ScanTargets(Vec<ScanTarget>),
    Discovered {
        batch: BatchId,
        targets: Vec<ScanTarget>,
    },
    DiscoveryFailed {
        batch: BatchId,
        error: String,
    },
    Finished {
        token: ScanToken,
        outcome: ScanOutcome,
    },
    Stop,
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum Effect {
    Discover {
        batch: BatchId,
    },
    Start {
        token: ScanToken,
        target: Box<ScanTarget>,
    },
    Cancel {
        token: ScanToken,
    },
}

#[derive(Default)]
pub struct ScanBatch {
    phase: BatchPhase,
    next_batch: u64,
    next_request: u64,
    batch: Option<BatchId>,
    active: Option<ActiveScan>,
    queue: VecDeque<ScanTarget>,
    statuses: BTreeMap<CardId, CardStatus>,
    total: usize,
    completed: usize,
    discovery_error: Option<String>,
}

impl ScanBatch {
    pub fn phase(&self) -> BatchPhase {
        self.phase
    }
    pub fn is_busy(&self) -> bool {
        self.phase() != BatchPhase::Idle
    }
    pub fn active(&self) -> Option<&ActiveScan> {
        self.active.as_ref()
    }
    pub fn accepts(&self, token: ScanToken) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.token == token && active.target.card_id == token.card_id)
    }
    pub fn status(&self, card: CardId) -> Option<&CardStatus> {
        self.statuses.get(&card)
    }
    pub fn queued_total(&self) -> usize {
        self.queue.len()
    }
    pub fn stopping(&self) -> bool {
        self.phase() == BatchPhase::Stopping
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn completed(&self) -> usize {
        self.completed
    }
    pub fn discovery_error(&self) -> Option<&str> {
        self.discovery_error.as_deref()
    }

    pub fn reduce(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::ScanAll if !self.is_busy() => {
                let batch = self.new_batch();
                self.phase = BatchPhase::Discovering;
                vec![Effect::Discover { batch }]
            }
            Event::ScanTargets(targets) if !self.is_busy() => {
                self.new_batch();
                self.freeze(targets);
                self.start_next()
            }
            Event::Discovered { batch, targets }
                if self.phase == BatchPhase::Discovering && self.batch == Some(batch) =>
            {
                self.freeze(targets);
                self.start_next()
            }
            Event::DiscoveryFailed { batch, error }
                if self.phase == BatchPhase::Discovering && self.batch == Some(batch) =>
            {
                self.discovery_error = Some(error);
                self.phase = BatchPhase::Idle;
                self.batch = None;
                vec![]
            }
            Event::Finished { token, outcome } if self.accepts(token) => {
                self.active = None;
                self.completed += 1;
                let status = match outcome {
                    ScanOutcome::Published { scan, partial } => CardStatus::Ready { scan, partial },
                    ScanOutcome::Failed(error) => CardStatus::Failed(error),
                    ScanOutcome::Cancelled => CardStatus::Cancelled,
                };
                self.statuses.insert(token.card_id, status);
                if matches!(self.phase, BatchPhase::Stopping | BatchPhase::Shutdown) {
                    self.batch = None;
                    if self.phase != BatchPhase::Shutdown {
                        self.phase = BatchPhase::Idle;
                    }
                    vec![]
                } else {
                    self.start_next()
                }
            }
            Event::Stop => self.stop(false),
            Event::Shutdown => self.stop(true),
            // Busy acquisition, duplicate acknowledgements, stale discovery and
            // cross-card/request callbacks are inert, even across new batches.
            _ => vec![],
        }
    }

    fn new_batch(&mut self) -> BatchId {
        self.next_batch = self
            .next_batch
            .checked_add(1)
            .expect("batch identity exhausted");
        let batch = BatchId(self.next_batch);
        self.batch = Some(batch);
        self.total = 0;
        self.completed = 0;
        self.discovery_error = None;
        batch
    }

    fn freeze(&mut self, targets: Vec<ScanTarget>) {
        let mut cards = BTreeSet::new();
        let mut roots = Vec::new();
        self.queue.clear();
        for target in targets {
            if !cards.insert(target.card_id) {
                continue;
            }
            if !same_root(&target.expected_root, &target.expected_root) {
                self.statuses.insert(
                    target.card_id,
                    CardStatus::Failed(
                        "The selected root identity cannot be verified. Choose it again.".into(),
                    ),
                );
                continue;
            }
            if roots
                .iter()
                .any(|identity| same_root(identity, &target.expected_root))
            {
                continue;
            }
            roots.push(target.expected_root.clone());
            self.queue.push_back(target);
        }
        self.total = self.queue.len();
        for (index, target) in self.queue.iter().enumerate() {
            self.statuses.insert(
                target.card_id,
                CardStatus::Queued {
                    ordinal: index + 1,
                    total: self.total(),
                },
            );
        }
    }

    fn start_next(&mut self) -> Vec<Effect> {
        debug_assert!(self.active.is_none());
        if let Some(target) = self.queue.pop_front() {
            self.next_request = self
                .next_request
                .checked_add(1)
                .expect("request identity exhausted");
            let token = ScanToken {
                batch: self.batch.expect("queued batch"),
                card_id: target.card_id,
                request: RequestId(self.next_request),
            };
            let ordinal = self.completed() + 1;
            self.statuses.insert(
                target.card_id,
                CardStatus::Scanning {
                    ordinal,
                    total: self.total(),
                },
            );
            self.active = Some(ActiveScan {
                token,
                target: target.clone(),
                ordinal,
                total: self.total(),
            });
            self.phase = BatchPhase::Running;
            vec![Effect::Start {
                token,
                target: Box::new(target),
            }]
        } else {
            self.phase = BatchPhase::Idle;
            self.batch = None;
            vec![]
        }
    }

    fn stop(&mut self, shutdown: bool) -> Vec<Effect> {
        for target in self.queue.drain(..) {
            self.statuses.insert(target.card_id, CardStatus::Cancelled);
        }
        let already_stopping = matches!(self.phase, BatchPhase::Stopping | BatchPhase::Shutdown);
        if shutdown || self.phase == BatchPhase::Shutdown {
            self.phase = BatchPhase::Shutdown;
        } else if self.active.is_some() {
            self.phase = BatchPhase::Stopping;
        } else {
            self.phase = BatchPhase::Idle;
        }
        if let Some(active) = &self.active {
            self.statuses
                .insert(active.token.card_id, CardStatus::Stopping);
            if !already_stopping {
                return vec![Effect::Cancel {
                    token: active.token,
                }];
            }
        } else {
            // Discovery has no scan worker to acknowledge. Its batch token is
            // immediately invalidated and its eventual response is ignored.
            self.batch = None;
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use space_core::{
        IdentityScheme, IdentityStrength, NativePath, ObservedIdentity, RootIdentity,
    };
    use std::path::Path;

    fn target(id: u64) -> ScanTarget {
        ScanTarget {
            card_id: CardId(id),
            path: NativePath::from_path(Path::new(&format!("/disk-{id}"))),
            expected_root: RootIdentity {
                volume: ObservedIdentity {
                    scheme: IdentityScheme::UnixDevice,
                    value: vec![1],
                    strength: IdentityStrength::Observed,
                },
                directory: ObservedIdentity {
                    scheme: IdentityScheme::UnixInode,
                    value: id.to_le_bytes().to_vec(),
                    strength: IdentityStrength::Observed,
                },
            },
            refresh_root: None,
        }
    }
    fn started(effects: &[Effect]) -> ScanToken {
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Start { token, .. } => *token,
            _ => panic!("expected one start"),
        }
    }
    fn published(id: u64) -> ScanOutcome {
        ScanOutcome::Published {
            scan: ScanId::new(id).unwrap(),
            partial: false,
        }
    }

    #[test]
    fn one_action_runs_three_targets_serially_and_preserves_independent_results() {
        let mut batch = ScanBatch::default();
        let discovery = batch.reduce(Event::ScanAll);
        let [Effect::Discover { batch: id }] = discovery.as_slice() else {
            panic!("discovery")
        };
        let first = started(&batch.reduce(Event::Discovered {
            batch: *id,
            targets: vec![target(1), target(2), target(3)],
        }));
        assert_eq!(batch.active().unwrap().ordinal, 1);
        assert_eq!(batch.queued_total(), 2);
        assert!(batch.reduce(Event::ScanAll).is_empty());
        let second = started(&batch.reduce(Event::Finished {
            token: first,
            outcome: published(10),
        }));
        assert_eq!(second.card_id, CardId(2));
        assert_eq!(batch.active().unwrap().ordinal, 2);
        let third = started(&batch.reduce(Event::Finished {
            token: second,
            outcome: published(20),
        }));
        assert_eq!(third.card_id, CardId(3));
        assert!(
            batch
                .reduce(Event::Finished {
                    token: third,
                    outcome: published(30)
                })
                .is_empty()
        );
        assert_eq!(batch.phase(), BatchPhase::Idle);
        assert_eq!(batch.completed(), 3);
        for (card, scan) in [(1, 10), (2, 20), (3, 30)] {
            assert_eq!(
                batch.status(CardId(card)),
                Some(&CardStatus::Ready {
                    scan: ScanId::new(scan).unwrap(),
                    partial: false
                })
            );
        }
    }

    #[test]
    fn failure_advances_and_stale_callbacks_cannot_cross_card_or_request_or_batch() {
        let mut batch = ScanBatch::default();
        let first = started(&batch.reduce(Event::ScanTargets(vec![target(1), target(2)])));
        for token in [
            ScanToken {
                card_id: CardId(2),
                ..first
            },
            ScanToken {
                batch: BatchId(first.batch.0 + 1),
                ..first
            },
            ScanToken {
                request: RequestId(first.request.0 + 1),
                ..first
            },
        ] {
            assert!(
                batch
                    .reduce(Event::Finished {
                        token,
                        outcome: published(99)
                    })
                    .is_empty()
            );
            assert!(batch.accepts(first));
        }
        let second = started(&batch.reduce(Event::Finished {
            token: first,
            outcome: ScanOutcome::Failed("Disconnected".into()),
        }));
        assert!(matches!(
            batch.status(CardId(1)),
            Some(CardStatus::Failed(_))
        ));
        batch.reduce(Event::Finished {
            token: second,
            outcome: published(2),
        });
        let next = started(&batch.reduce(Event::ScanTargets(vec![target(1)])));
        assert!(next.batch.0 > second.batch.0);
        assert!(next.request.0 > second.request.0);
        assert!(
            batch
                .reduce(Event::Finished {
                    token: first,
                    outcome: published(1)
                })
                .is_empty()
        );
        assert!(batch.accepts(next));
    }

    #[test]
    fn stop_discards_queue_and_waits_for_acknowledgement_even_when_commit_won() {
        for outcome in [
            ScanOutcome::Cancelled,
            published(1),
            ScanOutcome::Failed("late failure".into()),
        ] {
            let mut batch = ScanBatch::default();
            let token = started(&batch.reduce(Event::ScanTargets(vec![target(1), target(2)])));
            assert!(
                matches!(batch.reduce(Event::Stop).as_slice(), [Effect::Cancel { token: t }] if *t == token)
            );
            assert!(batch.stopping());
            assert_eq!(batch.queued_total(), 0);
            assert_eq!(batch.status(CardId(2)), Some(&CardStatus::Cancelled));
            assert!(batch.reduce(Event::Stop).is_empty());
            assert!(batch.reduce(Event::ScanTargets(vec![target(3)])).is_empty());
            assert!(batch.accepts(token));
            assert!(
                batch
                    .reduce(Event::Finished {
                        token,
                        outcome: outcome.clone()
                    })
                    .is_empty()
            );
            assert_eq!(batch.phase(), BatchPhase::Idle);
            if matches!(outcome, ScanOutcome::Published { .. }) {
                assert!(matches!(
                    batch.status(CardId(1)),
                    Some(CardStatus::Ready { .. })
                ));
            }
        }
    }

    #[test]
    fn stop_before_discovery_or_after_completion_does_not_start_another_worker() {
        let mut batch = ScanBatch::default();
        let effect = batch.reduce(Event::ScanAll);
        let [Effect::Discover { batch: old }] = effect.as_slice() else {
            panic!("discovery")
        };
        batch.reduce(Event::Stop);
        assert!(
            batch
                .reduce(Event::Discovered {
                    batch: *old,
                    targets: vec![target(1)]
                })
                .is_empty()
        );
        let token = started(&batch.reduce(Event::ScanTargets(vec![target(2)])));
        batch.reduce(Event::Finished {
            token,
            outcome: published(2),
        });
        assert!(batch.reduce(Event::Stop).is_empty());
        assert!(matches!(
            batch.status(CardId(2)),
            Some(CardStatus::Ready { .. })
        ));
    }

    #[test]
    fn shutdown_is_terminal_and_never_advances_the_queue() {
        let mut batch = ScanBatch::default();
        let token = started(&batch.reduce(Event::ScanTargets(vec![target(1), target(2)])));
        assert!(matches!(
            batch.reduce(Event::Shutdown).as_slice(),
            [Effect::Cancel { .. }]
        ));
        assert!(
            batch
                .reduce(Event::Finished {
                    token,
                    outcome: published(1)
                })
                .is_empty()
        );
        assert_eq!(batch.phase(), BatchPhase::Shutdown);
        assert!(batch.reduce(Event::ScanAll).is_empty());
        assert!(batch.reduce(Event::Stop).is_empty());
        assert_eq!(batch.phase(), BatchPhase::Shutdown);
    }

    #[test]
    fn frozen_native_target_survives_later_discovery_and_deduplicates_aliases_only() {
        let mut batch = ScanBatch::default();
        let mut alias = target(1);
        alias.card_id = CardId(3);
        let effects = batch.reduce(Event::ScanTargets(vec![target(1), alias, target(2)]));
        let token = started(&effects);
        assert_eq!(batch.total(), 2);
        assert!(
            batch
                .reduce(Event::Discovered {
                    batch: token.batch,
                    targets: vec![target(9)]
                })
                .is_empty()
        );
        let next = batch.reduce(Event::Finished {
            token,
            outcome: published(1),
        });
        let [Effect::Start { target: frozen, .. }] = next.as_slice() else {
            panic!("start")
        };
        assert_eq!(frozen.card_id, CardId(2));
        assert_eq!(frozen.expected_root, target(2).expected_root);
        assert_eq!(frozen.path, target(2).path);
    }

    #[test]
    fn discovery_failures_and_empty_or_weak_batches_leave_acquisition_available() {
        let mut batch = ScanBatch::default();
        let effects = batch.reduce(Event::ScanAll);
        let [Effect::Discover { batch: id }] = effects.as_slice() else {
            panic!("discovery")
        };
        batch.reduce(Event::DiscoveryFailed {
            batch: *id,
            error: "Metadata unavailable".into(),
        });
        assert_eq!(batch.discovery_error(), Some("Metadata unavailable"));
        assert!(!batch.is_busy());
        let mut weak = target(1);
        weak.expected_root.volume.strength = IdentityStrength::Weak;
        assert!(batch.reduce(Event::ScanTargets(vec![weak])).is_empty());
        assert!(!batch.is_busy());
        assert!(matches!(
            batch.status(CardId(1)),
            Some(CardStatus::Failed(_))
        ));
        assert!(batch.reduce(Event::ScanTargets(vec![])).is_empty());
        assert!(!batch.is_busy());
    }
}
