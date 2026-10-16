//! Regression journeys through the real Space event handlers and GPUI executor.
//! The test platform skips drawing only; navigation, asynchronous filtering,
//! cached queries, confirmation responses, and SQLite pin lifetimes are real.
use super::*;
use gpui::{TestAppContext, WindowHandle};
use space_core::ScanId;
use std::sync::atomic::AtomicU64;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

// Construct the production entity without Space::new's platform cache discovery.
// This fixture changes only initialization: every transition calls production code.
fn empty_space(cx: &mut Context<Space>) -> Space {
    let mut app = Space {
        disks: dashboard_controller::State::default(),
        changes: changes_controller::State::new(cx.background_executor().clone()),
        executor: cx.background_executor().clone(),
        filters_visible: false,
        cache_visible: false,
        cache_model: cache_management::Model::default(),
        cache_before: None,
        cache_epoch: 0,
        maintenance_busy: false,
        volumes: None,
        focus: cx.focus_handle(),
        list_focus: cx.focus_handle(),
        scroll: UniformListScrollHandle::new(),
        backend: None,
        cache_directory: None,
        snapshot: None,
        pending_snapshot: None,
        roots: vec![],
        intent: intent::Intent::default(),
        picker_open: false,
        running: None,
        generation: 0,
        status: "Opening local cache…".into(),
        view: View::Treemap,
        metric: Metric::Logical,
        applied_metric: Metric::Logical,
        applied_filter: space_store::QueryFilter::default(),
        breadcrumbs: vec![],
        rows: vec![],
        row_paths: vec![],
        alias_nodes: vec![],
        alias_origin: None,
        alias_next: None,
        alias_selected: None,
        alias_busy: false,
        alias_generation: 0,
        selected: 0,
        next_page: None,
        query_generation: 0,
        query_busy: false,
        pending_query: None,
        snapshot_request: 0,
        filter_inputs: [
            ("Name contains", "literal text"),
            ("Path contains", "relative display path"),
            ("Extension", "e.g. rs"),
            ("Minimum bytes", "0"),
            ("Maximum bytes", "unbounded"),
            ("Modified from UTC", "YYYY-MM-DD / RFC3339"),
            ("Modified through UTC", "YYYY-MM-DD / RFC3339"),
        ]
        .into_iter()
        .map(|(label, placeholder)| cx.new(|cx| text_input::TextInput::new(label, placeholder, cx)))
        .collect(),
        filter_subscriptions: vec![],
        filter_config: space_store::QueryFilter::default(),
        draft_unknown_size: false,
        draft_unknown_time: false,
        filter_session: None,
        filter_epoch: 0,
        filter_busy: false,
        filter_pending: None,
        filter_cancel: None,
        filter_status: "Unfiltered totals".into(),
        filter_error: None,
        subtree_only: true,
        treemap: treemap::Model::default(),
        current_totals: None,
        chart_selected: None,
    };
    app.disks.initializing = false;
    app
}

struct Journey {
    cx: TestAppContext,
    window: WindowHandle<Space>,
    directory: PathBuf,
    scans: [ScanId; 2],
}
impl Journey {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "space-gpui-transitions-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let cache_directory = directory.join("cache");
        let opened = backend::open(&cache_directory).unwrap();
        let mut roots = Vec::new();
        let mut scans = Vec::new();
        for (index, name) in ["alpha", "bravo"].into_iter().enumerate() {
            let source = directory.join(name);
            let nested = source.join(format!("{name}-folder"));
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(source.join(format!("{name}.bin")), vec![0; 11 + index * 12]).unwrap();
            for entry in 0..105 {
                std::fs::write(nested.join(format!("{name}-{entry:03}.bin")), [0]).unwrap();
            }
            let completed = backend::scan(
                &opened.backend,
                backend::ScanRequest {
                    path: source,
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
            roots = completed.roots;
            scans.push(completed.snapshot.scan);
        }
        let mut cx = TestAppContext::single();
        cx.skip_drawing();
        let window = cx.add_window(|_, cx| empty_space(cx));
        window
            .update(&mut cx, |app, _, cx| {
                let executor = cx.background_executor().clone();
                app.backend = Some(deferred::Deferred::new(
                    Arc::new(opened.backend),
                    move |backend| {
                        executor
                            .spawn(async move {
                                drop(backend);
                            })
                            .detach();
                    },
                ));
                app.cache_directory = Some(cache_directory);
                app.roots = roots;
                app.reconcile_disks(cx);
            })
            .unwrap();
        let mut journey = Self {
            cx,
            window,
            directory,
            scans: scans.try_into().unwrap(),
        };
        journey.settle();
        journey.read(|app| {
            assert!(app.disks.home);
            assert!(app.disks.selected.is_none());
            assert_eq!(app.dashboard_model().cards.len(), 2);
            assert!(
                app.dashboard_model()
                    .cards
                    .iter()
                    .all(|card| card.preview.is_some())
            );
        });
        journey
    }
    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut Space, &mut Window, &mut Context<Space>) -> R,
    ) -> R {
        self.window.update(&mut self.cx, f).unwrap()
    }
    fn read<R>(&self, f: impl FnOnce(&Space) -> R) -> R {
        self.window.read_with(&self.cx, |app, _| f(app)).unwrap()
    }
    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.read(|app| {
            assert!(!app.filter_busy, "filter worker did not finish");
            assert!(!app.query_busy, "query worker did not finish");
            assert!(!app.disks.preview_busy, "preview worker did not finish");
        });
    }
    fn ticket(&self, index: usize) -> dashboard::Ticket {
        self.read(|app| {
            app.dashboard_model()
                .cards
                .iter()
                .find(|card| card.ticket.scan == Some(self.scans[index]))
                .unwrap()
                .ticket
        })
    }
    fn open(&mut self, index: usize) {
        let ticket = self.ticket(index);
        self.update(|app, window, cx| {
            app.dashboard_event(dashboard::Event::Explore(ticket), window, cx)
        });
    }
    fn action(&mut self, action: explorer::Action) {
        self.update(|app, window, cx| {
            let event = app.explorer_model().event(action);
            assert!(app.explorer_model().allows(event));
            app.explorer_event(event, window, cx);
        });
    }
    fn filtered_alpha(&mut self) {
        self.open(0);
        self.settle();
        self.update(|app, _, cx| {
            app.filter_config = space_store::QueryFilter {
                extension: Some("bin".into()),
                ..Default::default()
            };
            app.rebuild_filters(cx);
        });
        self.settle();
        self.assert_display(0);
        self.read(|app| assert!(app.filter_session.is_some()));
    }
    fn assert_display(&self, index: usize) {
        let ticket = self.ticket(index);
        self.read(|app| {
            let snapshot = app.snapshot.as_ref().unwrap();
            assert!(!app.disks.home);
            assert_eq!(app.disks.selected, Some(ticket.card));
            assert_eq!(snapshot.scan, self.scans[index]);
            let model = app.explorer_model();
            assert_eq!(model.ticket.unwrap().card, ticket.card);
            assert_eq!(model.ticket.unwrap().scan, Some(snapshot.scan));
            assert_eq!(model.name, if index == 0 { "alpha" } else { "bravo" });
            assert!(
                model
                    .location
                    .ends_with(if index == 0 { "alpha" } else { "bravo" })
            );
            assert_eq!(
                app.breadcrumbs.first().unwrap().0,
                snapshot.root_entry.entry.node
            );
        });
    }
}
impl Drop for Journey {
    fn drop(&mut self) {
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.remove_window());
        self.cx.run_until_parked();
        self.cx.quit();
        self.cx.run_until_parked();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn cancelled_filtered_card_switch_retains_previous_card_snapshot_and_map() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    let before = journey.read(|app| {
        (
            app.breadcrumbs.clone(),
            app.treemap
                .items
                .iter()
                .map(|item| (item.node, item.label.clone(), item.bytes))
                .collect::<Vec<_>>(),
        )
    });
    let next_scan = journey.scans[1];
    journey.open(1);
    journey.assert_display(0);
    journey.read(|app| {
        assert!(app.filter_busy);
        assert_eq!(app.pending_snapshot.as_ref().unwrap().scan, next_scan);
    });
    journey.update(|app, window, cx| app.cancel_filter(&CancelFilter, window, cx));
    journey.settle();
    journey.assert_display(0);
    journey.read(|app| {
        assert!(app.pending_snapshot.is_none());
        assert_eq!(app.breadcrumbs, before.0);
        assert_eq!(
            app.treemap
                .items
                .iter()
                .map(|item| (item.node, item.label.clone(), item.bytes))
                .collect::<Vec<_>>(),
            before.1
        );
        assert!(app.filter_status.starts_with("Filter cancelled"));
    });
    // Cancellation must also discard B's staged bookmark, not merely its snapshot.
    journey.open(0);
    journey.settle();
    journey.assert_display(0);
}

#[test]
fn filtered_card_switch_commits_identity_snapshot_and_scoped_query_together() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    let next = journey.ticket(1);
    journey.action(explorer::Action::Select(next));
    journey.assert_display(0);
    journey.settle();
    journey.assert_display(1);
    journey.read(|app| {
        assert!(app.pending_snapshot.is_none());
        assert_eq!(app.applied_filter.extension.as_deref(), Some("bin"));
        assert!(
            app.rows
                .iter()
                .all(|row| row.entry.display_name.starts_with("bravo"))
        );
        assert!(
            app.treemap
                .items
                .iter()
                .all(|item| item.label.starts_with("bravo"))
        );
        assert_eq!(
            app.current_totals.as_ref().unwrap().logical.observed_bytes,
            128
        );
    });
}

#[test]
fn failed_filter_completion_does_not_move_explorer_to_the_requested_card() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    journey.update(|app, _, _| {
        // Exercise the real background error callback, rather than a reducer stub.
        app.filter_config.min_size = Some(100);
        app.filter_config.max_size = Some(1);
    });
    journey.open(1);
    journey.assert_display(0);
    journey.settle();
    journey.assert_display(0);
    journey.read(|app| {
        assert!(app.pending_snapshot.is_none());
        assert!(app.filter_status.starts_with("Filter failed"));
        assert!(
            app.rows
                .iter()
                .all(|row| row.entry.display_name.starts_with("alpha"))
        );
    });
}

#[test]
fn filtered_return_restores_the_saved_folder_page_and_selection() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    let folder = journey.read(|app| {
        app.treemap
            .items
            .iter()
            .find(|item| item.is_directory)
            .unwrap()
            .node
    });
    journey.update(|app, window, cx| app.chart_event(treemap::Event::Open(folder), window, cx));
    journey.settle();
    journey.action(explorer::Action::View(explorer::View::Folders));
    journey.settle();
    journey.update(|app, window, cx| app.next_page(&NextPage, window, cx));
    journey.settle();
    journey.update(|app, _, cx| app.move_selection(2, cx));
    let before = journey.read(|app| {
        assert_eq!(app.rows.len(), 5);
        let cursor = app.disks.page_cursor.unwrap();
        (
            app.breadcrumbs.clone(),
            (cursor.ordinal, cursor.value),
            app.rows[app.selected].entry.node,
        )
    });
    journey.open(1);
    journey.settle();
    journey.assert_display(1);
    journey.open(0);
    journey.settle();
    journey.assert_display(0);
    journey.read(|app| {
        let cursor = app.disks.page_cursor.unwrap();
        assert_eq!(app.breadcrumbs, before.0);
        assert_eq!((cursor.ordinal, cursor.value), before.1);
        assert_eq!(app.rows.len(), 5);
        assert_eq!(app.rows[app.selected].entry.node, before.2);
        assert_eq!(app.view, View::Folders);
        assert_eq!(app.breadcrumbs.last().unwrap().0, folder);
    });
}

#[test]
fn cache_removal_reloads_remaining_snapshot_without_reusing_removed_card_identity() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    journey.action(explorer::Action::Cache);
    journey.settle();
    let removed = journey.scans[0];
    let remaining = journey.scans[1];
    journey.update(|app, window, cx| {
        assert!(app.cache_idle());
        assert!(
            app.cache_model
                .allows(cache_management::Event::RemoveRequested(removed))
        );
        app.confirm_cache_removal(Some(removed), window, cx);
    });
    assert!(journey.cx.has_pending_prompt());
    journey.cx.simulate_prompt_answer("Remove cached metadata");
    journey.settle();
    journey.read(|app| {
        assert!(!app.maintenance_busy);
        assert!(app.disks.home);
        assert!(app.disks.selected.is_none());
        assert!(app.explorer_model().ticket.is_none());
        assert_eq!(app.snapshot.as_ref().unwrap().scan, remaining);
        assert_eq!(app.roots.len(), 1);
        assert!(
            app.dashboard_model()
                .cards
                .iter()
                .all(|card| card.ticket.scan != Some(removed))
        );
        assert!(
            app.cache_model
                .warning
                .as_deref()
                .unwrap()
                .starts_with("Cached metadata removed")
        );
    });
    journey.open(1);
    journey.settle();
    journey.assert_display(1);
}

#[test]
fn superseded_filter_callback_cannot_commit_an_older_navigation() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    journey.open(1);
    journey.read(|app| assert!(app.filter_busy));
    journey.open(0);
    journey.assert_display(0);
    journey.read(|app| {
        assert!(app.filter_pending.is_some());
        assert!(app.pending_snapshot.is_some());
    });
    journey.settle();
    journey.assert_display(0);
    journey.read(|app| {
        assert!(app.filter_pending.is_none());
        assert!(app.pending_snapshot.is_none());
        assert!(
            app.rows
                .iter()
                .all(|row| row.entry.display_name.starts_with("alpha"))
        );
    });
}

#[test]
fn independent_cached_maps_open_the_correct_offline_folder_and_keep_advanced_actions() {
    let mut journey = Journey::new();
    std::fs::remove_dir_all(journey.directory.join("alpha")).unwrap();
    std::fs::remove_dir_all(journey.directory.join("bravo")).unwrap();
    for (index, name) in ["alpha", "bravo"].into_iter().enumerate() {
        let ticket = journey.ticket(index);
        let folder = journey.read(|app| {
            let model = app.dashboard_model();
            let card = model
                .cards
                .iter()
                .find(|card| card.ticket == ticket)
                .unwrap();
            assert_eq!(card.status, dashboard::Status::Offline);
            let preview = card.preview.as_ref().unwrap();
            assert!(
                preview
                    .items
                    .iter()
                    .all(|item| item.label.starts_with(name))
            );
            preview
                .items
                .iter()
                .find(|item| item.is_directory)
                .unwrap()
                .node
        });
        journey.update(|app, window, cx| {
            app.dashboard_event(
                dashboard::Event::Map(ticket, treemap::Event::Open(folder)),
                window,
                cx,
            );
        });
        journey.settle();
        journey.assert_display(index);
        journey.read(|app| {
            assert_eq!(app.breadcrumbs.len(), 2);
            assert_eq!(app.breadcrumbs.last().unwrap().0, folder);
            assert!(
                app.rows
                    .iter()
                    .all(|row| row.entry.display_name.starts_with(name))
            );
            let model = app.explorer_model();
            assert!(model.offline);
            for action in [
                explorer::Action::Filters,
                explorer::Action::Changes,
                explorer::Action::Cache,
            ] {
                assert!(model.allows(model.event(action)));
            }
        });
        journey.action(explorer::Action::Home);
        journey.settle();
        journey.read(|app| assert!(app.disks.home));
    }
}

#[test]
fn sequential_background_publications_keep_the_displayed_snapshot_and_geometry() {
    let mut journey = Journey::new();
    journey.open(0);
    journey.settle();
    let before = journey.read(|app| {
        (
            app.disks.selected,
            app.snapshot.as_ref().unwrap().scan,
            app.treemap
                .items
                .iter()
                .map(|item| (item.node, item.label.clone(), item.bytes))
                .collect::<Vec<_>>(),
        )
    });
    std::fs::write(journey.directory.join("alpha/added.bin"), [0; 3]).unwrap();
    std::fs::write(journey.directory.join("bravo/added.bin"), [0; 7]).unwrap();
    journey.update(|app, _, cx| {
        let mut targets = Vec::new();
        // Revalidation is represented by the already verified fixture identities;
        // the real backend independently inspects each path before publication.
        for root in app.roots.clone().into_iter().rev() {
            let card = app.disks.registry.register_folder(
                root.path.clone(),
                root.identity.clone(),
                Some(root),
            );
            targets.push(app.disks.registry.target(card).unwrap());
        }
        app.batch_event(scan_batch::Event::ScanTargets(targets), cx);
        assert!(app.disks.batch.is_busy());
        assert_eq!(app.disks.batch.queued_total(), 1);
        assert!(app.running.is_some());
    });
    journey.settle();
    journey.read(|app| {
        assert!(!app.disks.batch.is_busy());
        assert!(app.running.is_none());
        assert_eq!(app.disks.selected, before.0);
        assert_eq!(app.snapshot.as_ref().unwrap().scan, before.1);
        assert_eq!(
            app.treemap
                .items
                .iter()
                .map(|item| (item.node, item.label.clone(), item.bytes))
                .collect::<Vec<_>>(),
            before.2
        );
        let dashboard = app.dashboard_model();
        for card in &dashboard.cards {
            assert!(!journey.scans.contains(&card.ticket.scan.unwrap()));
            assert!(
                card.preview
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .any(|item| item.label == "added.bin")
            );
            assert!(matches!(
                app.disks.batch.status(card.ticket.card),
                Some(scan_batch::CardStatus::Ready { .. })
            ));
        }
    });
}

#[test]
fn returning_home_while_a_card_filter_runs_cannot_be_undone_by_completion() {
    let mut journey = Journey::new();
    journey.filtered_alpha();
    let original = journey.read(|app| (app.disks.selected, app.snapshot.as_ref().unwrap().scan));
    journey.open(1);
    journey.read(|app| assert!(app.filter_busy && app.pending_snapshot.is_some()));
    journey.action(explorer::Action::Home);
    journey.read(|app| {
        assert!(app.disks.home);
        assert!(app.pending_snapshot.is_none());
    });
    journey.settle();
    journey.read(|app| {
        assert!(app.disks.home);
        assert!(app.pending_snapshot.is_none());
        assert_eq!(
            (app.disks.selected, app.snapshot.as_ref().unwrap().scan),
            original
        );
    });
    journey.open(0);
    journey.settle();
    journey.assert_display(0);
}
