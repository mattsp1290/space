//! Real scanner/store/controller journeys, with explicit frames for hit-testing.
use super::*;
use explore::Action;
use gpui::{TestAppContext, VisualTestContext, WindowHandle, point};
struct Journey {
    cx: TestAppContext,
    window: WindowHandle<Space>,
    fixture: test_fixture::Fixture,
}
impl Journey {
    fn new(drawn: bool) -> Self {
        let fixture = test_fixture::Fixture::new();
        let mut cx = TestAppContext::single();
        if !drawn {
            cx.skip_drawing();
        }
        cx.update(bind_keys);
        let window = cx.add_window(|_, cx| Space::empty(cx));
        let backend = fixture.backend.clone();
        let directory = fixture.cache_directory.clone();
        let roots = fixture.roots.clone();
        let discovery_roots = roots.clone();
        let volumes = ["alpha", "bravo"]
            .into_iter()
            .map(|name| {
                let path = fixture.directory.join(name);
                space_scan::MountedVolume {
                    label: name.into(),
                    mount_path: space_core::NativePath::from_path(&path),
                    aliases: vec![],
                    identity: space_scan::inspect_root(&path).unwrap(),
                    filesystem: "fixture".into(),
                    removable: None,
                    capacity: None,
                }
            })
            .collect::<Vec<_>>();
        let listing = space_scan::VolumeListing {
            observed_at: space_core::Timestamp {
                seconds: 0,
                nanos: 0,
            },
            volumes,
            issues: vec![],
            complete: true,
        };
        window
            .update(&mut cx, move |app, _, cx| {
                let executor = cx.background_executor().clone();
                app.backend = Some(deferred::Deferred::new(Arc::new(backend), move |value| {
                    executor
                        .spawn(async move {
                            drop(value);
                        })
                        .detach();
                }));
                app.cache_directory = Some(directory);
                app.roots = roots;
                app.volumes = Some(listing.clone());
                app.disks.discovery_override = Some(Arc::new(move || {
                    Ok((listing.clone(), discovery_roots.clone()))
                }));
                app.reconcile_disks(cx);
            })
            .unwrap();
        let mut result = Self {
            cx,
            window,
            fixture,
        };
        result.visual().simulate_resize(size(px(1280.), px(800.)));
        result.settle();
        result
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
        self.cx.executor().advance_clock(Duration::from_millis(300));
        self.cx.run_until_parked();
        self.read(|app| {
            assert!(!app.explore.tree_busy);
            assert!(app.explore.pending.is_none());
            assert!(!app.disks.preview_busy);
        });
    }
    fn frame(&mut self) {
        self.cx
            .update_window(*self.window, |_, w, cx| {
                w.draw(cx).clear(cx);
            })
            .unwrap();
    }
    fn visual(&self) -> VisualTestContext {
        VisualTestContext::from_window(*self.window, &self.cx)
    }
    fn ticket(&self, index: usize) -> ticket::Ticket {
        self.read(|app| {
            app.home_model()
                .cards
                .iter()
                .find(|c| c.ticket.scan == Some(self.fixture.scans[index]))
                .unwrap()
                .ticket
        })
    }
    fn open(&mut self, index: usize) {
        let ticket = self.ticket(index);
        self.update(|app, w, cx| app.home_event(home::Event::Explore(ticket), w, cx));
        self.settle();
    }
    fn action(&mut self, action: Action) {
        self.update(|app, w, cx| {
            let event = app.explore_model().event(action);
            assert!(app.explore_model().allows(&event), "{:?}", event.action);
            app.explore_event(event, w, cx);
        });
    }
    fn node(&self, label: &str) -> NodeId {
        self.read(|app| {
            app.explore
                .tree
                .as_ref()
                .unwrap()
                .children
                .iter()
                .find(|n| n.label == label)
                .unwrap()
                .node
        })
    }
}
impl Drop for Journey {
    fn drop(&mut self) {
        let _ = self
            .window
            .update(&mut self.cx, |_, w, _| w.remove_window());
        self.cx.run_until_parked();
        self.cx.quit();
        self.cx.run_until_parked();
    }
}
#[test]
fn d0_drawn_smoke_and_d1_map_dominance() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let first = j.visual().debug_bounds("explore-map").expect("drawn map");
    let area = f32::from(first.size.width) * f32::from(first.size.height);
    assert!(area >= 1280. * 800. * 0.62, "{first:?}");
    j.action(Action::ToggleList);
    j.settle();
    j.frame();
    let second = j.visual().debug_bounds("explore-map").unwrap();
    assert!(second.size.width > first.size.width);
    assert!(f32::from(second.size.width) * f32::from(second.size.height) >= 1280. * 800. * 0.75);
}
#[test]
fn j2_navigation_and_e4_up_restores_selection() {
    let mut j = Journey::new(false);
    j.open(0);
    let generation = j.read(|a| a.explore.query_generation);
    let mut nodes = vec![];
    for label in ["a", "b", "c"] {
        let node = j.node(label);
        nodes.push(node);
        j.action(Action::ZoomInto(node));
        j.settle();
    }
    j.read(|a| {
        assert_eq!(a.explore.crumbs.len(), 4);
        assert_eq!(a.explore.query_generation, generation + 3);
    });
    for node in nodes.into_iter().rev() {
        j.action(Action::Up);
        j.settle();
        j.read(|a| {
            assert_eq!(a.explore.selected, Some(node));
            assert_eq!(a.explore.selection_path.as_ref().map(|p| p.0), Some(node));
        });
    }
}
impl Journey {
    fn tile_point(&self, target: tree::Target, header: bool) -> gpui::Point<gpui::Pixels> {
        let bounds = self.visual().debug_bounds("explore-map").unwrap();
        let rect = self.read(|a| {
            let cache = a.explore.geometry.borrow();
            let tile = cache
                .as_ref()
                .unwrap()
                .geometry
                .tiles
                .iter()
                .find(|t| t.target == target)
                .unwrap()
                .clone();
            if header {
                tile.header.unwrap()
            } else {
                tile.body
            }
        });
        bounds.origin
            + point(
                px(rect.x + rect.width / 2.),
                px(rect.y
                    + if header {
                        rect.height / 2.
                    } else {
                        rect.height - 3.
                    }),
            )
    }
}
#[test]
fn d2_header_clicks_drill_three_levels_d4_focus_and_backspace() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    j.update(|a, w, _| {
        assert!(a.focus.is_focused(w));
        assert!(!a.explore.focus_pending);
    });
    for label in ["a", "b", "c"] {
        let node = j.node(label);
        let p = j.tile_point(tree::Target::Node(node), true);
        j.visual().simulate_click(p, Default::default());
        j.settle();
        j.frame();
        j.read(|a| assert_eq!(a.explore.tree.as_ref().unwrap().root, node));
    }
    j.read(|a| assert_eq!(a.explore.crumbs.len(), 4));
    j.visual().simulate_keystrokes("backspace");
    j.settle();
    j.frame();
    j.read(|a| assert_eq!(a.explore.crumbs.len(), 3));
}
#[test]
fn d3_real_hover_d6_tooltip_and_nested_selection() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let (point, target) = j.read(|a| {
        let cache = a.explore.geometry.borrow();
        let g = &cache.as_ref().unwrap().geometry;
        let tile = g
            .tiles
            .iter()
            .rev()
            .find(|t| t.depth >= 2 && t.rect.area() > 100.)
            .unwrap();
        let x = tile.rect.x + tile.rect.width / 2.;
        let y = tile.rect.y + tile.rect.height / 2.;
        let target = g.tiles[tree::hit_test(g, x, y).unwrap()].target;
        (point(px(x + 8.), px(y + 88.)), target)
    });
    let generation = j.read(|a| a.explore.query_generation);
    j.visual()
        .simulate_mouse_move(point, None, Default::default());
    j.read(|a| {
        assert_eq!(a.explore.hovered, Some(target));
        assert_eq!(a.explore.query_generation, generation);
        assert!(!a.explore.tree_busy);
    });
    j.frame();
    assert!(j.visual().debug_bounds("explore-tooltip").is_some());
    j.visual().simulate_click(point, Default::default());
    j.settle();
    j.read(|a| {
        if let tree::Target::Node(n) = target {
            assert_eq!(a.explore.selected, Some(n));
        }
    });
}
#[test]
fn d5_click_from_old_frame_is_ignored() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let node = j.node("a");
    let p = j.tile_point(tree::Target::Node(node), true);
    j.action(Action::ZoomInto(node));
    // Dispatch both native events before pumping the executor, preserving the
    // old rendered listeners while the controller already has a newer epoch.
    j.cx.update_window(*j.window, |_, w, cx| {
        w.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: p,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        w.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: p,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
            }),
            cx,
        );
    })
    .unwrap();
    j.read(|a| {
        assert_eq!(a.explore.crumbs.len(), 2);
        assert!(a.explore.selected.is_none());
    });
    j.settle();
    j.read(|a| assert_eq!(a.explore.tree.as_ref().unwrap().root, node));
}
#[test]
fn d7_disk_menu_occludes_map() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    j.action(Action::ToggleDiskMenu);
    j.settle();
    j.frame();
    let menu = j.visual().debug_bounds("disk-choices").unwrap();
    let p = menu.origin + point(px(5.), px(5.));
    j.visual().simulate_mouse_move(p, None, Default::default());
    j.read(|a| assert!(a.explore.hovered.is_none()));
    j.visual().simulate_click(p, Default::default());
    j.settle();
    j.read(|a| assert!(a.explore.selected.is_none()));
}
#[test]
fn j3_background_publication_preserves_display_and_offers_new_scan() {
    let mut j = Journey::new(false);
    j.open(0);
    let scan = j.read(|a| a.explore.snapshot.as_ref().unwrap().scan);
    let bravo = j.ticket(1);
    j.update(|a, _, cx| {
        let target = a.disks.registry.target(bravo.card).unwrap();
        a.batch_event(scan_batch::Event::ScanTargets(vec![target]), cx);
    });
    j.settle();
    j.read(|a| {
        assert_eq!(a.screen, Screen::Explore);
        assert_eq!(a.explore.snapshot.as_ref().unwrap().scan, scan);
        assert!(a.explore.new_scan_available.is_none());
    });
    j.action(Action::Rescan);
    j.settle();
    j.read(|a| {
        assert_eq!(a.explore.snapshot.as_ref().unwrap().scan, scan);
        assert!(a.explore.new_scan_available.is_some());
    });
    j.action(Action::ShowNewScan);
    j.settle();
    j.read(|a| {
        assert_ne!(a.explore.snapshot.as_ref().unwrap().scan, scan);
        assert!(a.explore.new_scan_available.is_none());
        assert_eq!(a.explore.crumbs.len(), 1);
    });
}
#[test]
fn j4_stale_ticket_generation_and_e3_hover_no_worker() {
    let mut j = Journey::new(false);
    j.open(0);
    let node = j.node("a");
    let old = j.read(|a| a.explore_model().event(Action::Select(Some(node))));
    j.action(Action::ZoomInto(node));
    j.settle();
    j.update(|a, w, cx| a.explore_event(old, w, cx));
    j.read(|a| assert!(a.explore.selected.is_none()));
    let target = j.read(|a| tree::Target::Node(a.explore.tree.as_ref().unwrap().children[0].node));
    let generation = j.read(|a| a.explore.query_generation);
    j.action(Action::Hover(Some(target)));
    j.action(Action::Hover(Some(target)));
    j.read(|a| {
        assert_eq!(a.explore.hovered, Some(target));
        assert_eq!(a.explore.query_generation, generation);
        assert!(!a.explore.tree_busy);
    });
    let event = j.read(|a| a.explore_model().event(Action::Up));
    j.open(1);
    j.update(|a, w, cx| a.explore_event(event, w, cx));
    j.read(|a| assert_eq!(a.explore.crumbs.len(), 1));
}
#[test]
fn e10_pending_scope_replaced_and_released() {
    let mut j = Journey::new(false);
    j.open(0);
    let (a, b) = j.read(|s| {
        let a = s
            .explore
            .tree
            .as_ref()
            .unwrap()
            .children
            .iter()
            .find(|n| n.label == "a")
            .unwrap();
        (
            a.node,
            a.children.iter().find(|n| n.label == "b").unwrap().node,
        )
    });
    j.update(|app, _, cx| {
        app.explore
            .crumbs
            .push((a, "a".into(), DisplayTotals::default()));
        app.enter_scope(a, None, None, cx);
        app.explore
            .crumbs
            .push((b, "b".into(), DisplayTotals::default()));
        app.enter_scope(b, None, None, cx);
    });
    j.settle();
    j.read(|s| {
        assert_eq!(s.explore.tree.as_ref().unwrap().root, b);
        assert!(!s.explore.tree_loading);
        assert!(!s.explore.tree_busy);
        assert!(s.explore.pending.is_none());
    });
}
#[test]
fn j5_bookmark_e5_list_sync_and_e4_crumb() {
    let mut j = Journey::new(false);
    j.open(0);
    let node = j.node("a");
    j.action(Action::Select(Some(node)));
    j.settle();
    j.read(|a| {
        assert_eq!(
            a.explore.list_rows[a.explore.list_selected.unwrap()]
                .entry
                .node,
            node
        )
    });
    j.action(Action::ZoomInto(node));
    j.settle();
    j.action(Action::ListSelect(0));
    j.settle();
    let selected = j.read(|a| a.explore.selected);
    j.open(1);
    j.open(0);
    j.read(|a| {
        assert_eq!(a.explore.tree.as_ref().unwrap().root, node);
        assert_eq!(a.explore.selected, selected);
        assert_eq!(a.explore.selection_path.as_ref().map(|p| p.0), selected);
    });
    let root = j.read(|a| a.explore.crumbs[0].0);
    j.action(Action::Crumb(root));
    j.settle();
    j.read(|a| assert_eq!(a.explore.crumbs.len(), 1));
}
#[test]
fn j6_name_search_clear_e9_staleness_e11_metric() {
    let mut j = Journey::new(false);
    j.open(0);
    let root = j.read(|a| a.explore.tree.as_ref().unwrap().root);
    let total = j.read(|a| a.explore.tree.as_ref().unwrap().total_bytes);
    j.update(|a, _, cx| {
        a.search_changed("data-001".into(), cx);
        a.search_changed("alpha-001".into(), cx);
    });
    j.settle();
    j.read(|a| {
        assert_eq!(a.explore.search.matching, Some(1));
        assert_eq!(a.explore.tree.as_ref().unwrap().root, root);
        assert!(a.explore.tree.as_ref().unwrap().total_bytes < total);
        assert!(!a.explore.search.busy);
    });
    let epoch = j.read(|a| a.explore.search.epoch);
    j.action(Action::ToggleMetric);
    j.settle();
    j.read(|a| {
        assert!(a.explore.search.epoch > epoch);
        assert_eq!(a.explore.tree.as_ref().unwrap().metric, Metric::Allocated);
        assert_eq!(a.explore.search.matching, Some(1));
    });
    j.action(Action::SearchClear);
    j.settle();
    j.action(Action::ToggleMetric);
    j.settle();
    j.read(|a| assert_eq!(a.explore.tree.as_ref().unwrap().total_bytes, total));
}
#[test]
fn j1_one_click_scans_all_discovered_fixture_disks() {
    let mut j = Journey::new(false);
    let cache = j.fixture.directory.join("fresh-cache");
    let backend = backend::open(&cache).unwrap().backend;
    j.update(|a, _, cx| {
        let listing = a.volumes.clone().unwrap();
        a.disks = Default::default();
        a.disks.discovery_override = Some(Arc::new(move || Ok((listing.clone(), vec![]))));
        let executor = a.executor.clone();
        a.backend = Some(deferred::Deferred::new(Arc::new(backend), move |value| {
            executor
                .spawn(async move {
                    drop(value);
                })
                .detach();
        }));
        a.cache_directory = Some(cache);
        a.roots.clear();
        a.reconcile_disks(cx);
    });
    j.settle();
    j.read(|a| {
        assert_eq!(a.home_model().cards.len(), 2);
        assert!(a.home_model().cards.iter().all(|c| c.ticket.scan.is_none()));
        assert_eq!(a.home_model().primary().0, "Scan all disks");
    });
    j.update(|a, w, cx| a.home_event(home::Event::ScanAll, w, cx));
    j.settle();
    j.read(|a| {
        assert_eq!(a.screen, Screen::Home);
        assert!(
            a.home_model()
                .cards
                .iter()
                .all(|c| c.status == home::Status::Ready
                    && c.preview.as_ref().is_some_and(|t| !t.children.is_empty()))
        );
        assert_eq!(a.disks.batch.completed(), 2);
    });
}
#[test]
fn j5_stop_retains_publication_and_h6_explore_before_preview() {
    let mut j = Journey::new(false);
    let t = j.ticket(0);
    j.update(|a, w, cx| {
        a.disks.cards.get_mut(&t.card).unwrap().snapshot = None;
        a.home_event(home::Event::Explore(t), w, cx);
        assert!(a.disks.cards[&t.card].opening);
    });
    j.settle();
    j.read(|a| {
        assert_eq!(a.screen, Screen::Explore);
        assert!(a.explore.tree.is_some());
        assert!(!a.disks.cards[&t.card].opening);
        assert!(a.explore.focus_pending);
    });
    j.action(Action::Home);
    j.settle();
    let before = j.read(|a| a.home_model().cards[0].preview.clone().unwrap());
    j.update(|a, w, cx| {
        a.home_event(home::Event::ScanAll, w, cx);
        a.home_event(home::Event::Stop, w, cx);
    });
    j.settle();
    j.read(|a| {
        let m = a.home_model();
        assert_eq!(m.primary().0, "Rescan all disks");
        assert!(
            m.cards[0]
                .preview
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &before))
        );
    });
}
#[test]
fn e1_validation_and_e9_inflight_search_cannot_cross_disk() {
    let mut j = Journey::new(false);
    j.open(0);
    let invalid = NodeId::new(i64::MAX as u64).unwrap();
    j.read(|a| {
        let m = a.explore_model();
        assert!(!m.allows(&m.event(Action::Up)));
        assert!(!m.allows(&m.event(Action::ZoomInto(invalid))));
        assert!(!m.allows(&m.event(Action::Select(Some(invalid)))));
        assert!(!m.allows(&m.event(Action::ShowNewScan)));
    });
    j.update(|a, _, cx| {
        a.search_changed("data-001".into(), cx);
        let epoch = a.explore.search.epoch;
        a.start_search(epoch, cx);
    });
    j.open(1);
    j.settle();
    j.read(|a| {
        assert!(a.explore.search.text.is_empty());
        assert!(a.explore.search.session.is_none());
        assert!(a.explore.search.matching.is_none());
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            j.fixture.scans[1]
        );
    });
}
#[test]
fn d7_preview_geometries_are_independent() {
    let mut j = Journey::new(true);
    j.frame();
    j.read(|a| {
        let m = a.home_model();
        let left = m.cards[0].geometry.borrow();
        let right = m.cards[1].geometry.borrow();
        let left = left.as_ref().unwrap();
        let right = right.as_ref().unwrap();
        assert!(!Arc::ptr_eq(&left.tree, &right.tree));
        assert_ne!(left.geometry.tiles, right.geometry.tiles);
    });
}
#[test]
fn h7_cards_beyond_preview_cap_open_and_release_previous_pin() {
    let mut j = Journey::new(true);
    let mut roots = j.fixture.roots.clone();
    for i in 0..24 {
        let path = j.fixture.directory.join(format!("saved-{i}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("file"), [0]).unwrap();
        roots = backend::scan(
            &j.fixture.backend,
            backend::ScanRequest {
                path,
                cache_directory: j.fixture.cache_directory.clone(),
                expected_root: None,
                refresh_root: None,
                generation: 100 + i,
            },
            &Cancellation::new(),
            &backend::Progress::default(),
        )
        .unwrap()
        .unwrap()
        .roots;
    }
    j.update(|a, _, cx| {
        a.roots = roots;
        a.reconcile_disks(cx);
    });
    j.settle();
    j.frame();
    let tickets = j.read(|a| {
        let m = a.home_model();
        assert_eq!(m.cards.len(), 26);
        assert!(m.cards[24].preview.is_none());
        assert!(m.cards[25].preview.is_none());
        [m.cards[24].ticket, m.cards[25].ticket]
    });
    j.update(|a, w, cx| a.home_event(home::Event::Explore(tickets[0]), w, cx));
    j.settle();
    j.read(|a| {
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            tickets[0].scan.unwrap()
        );
        assert!(a.disks.cards[&tickets[0].card].snapshot.is_some());
    });
    j.update(|a, w, cx| a.home_event(home::Event::Explore(tickets[1]), w, cx));
    j.settle();
    j.read(|a| {
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            tickets[1].scan.unwrap()
        );
        assert!(a.disks.cards[&tickets[0].card].snapshot.is_none());
        assert!(a.disks.cards[&tickets[1].card].snapshot.is_some());
    });
    let outside_root = j.read(|a| {
        a.disks
            .registry
            .card(tickets[0].card)
            .unwrap()
            .root
            .clone()
            .unwrap()
    });
    let outside =
        backend::open_scan(&j.fixture.backend, outside_root, tickets[0].scan.unwrap()).unwrap();
    let expected = tree::format_bytes(metric_bytes(&outside.root_entry, Metric::Allocated));
    j.action(Action::ToggleMetric);
    j.settle();
    j.read(|a| {
        assert_eq!(
            a.home_model()
                .cards
                .iter()
                .find(|c| c.ticket.card == tickets[0].card)
                .unwrap()
                .cached_total,
            format!("{expected} observed")
        )
    });
    j.open(0);
    j.frame();
    let selected = j.ticket(0).card;
    let displaced = j.ticket(1).card;
    let roots = j.read(|a| a.roots.clone());
    let volumes = roots
        .iter()
        .filter(|r| r.id != j.fixture.roots[0].id && r.id != j.fixture.roots[1].id)
        .map(|r| space_scan::MountedVolume {
            label: "Saved source".into(),
            mount_path: r.path.clone(),
            aliases: vec![],
            identity: r.identity.clone(),
            filesystem: "fixture".into(),
            removable: None,
            capacity: None,
        })
        .collect();
    j.update(|a, _, cx| {
        a.volumes.as_mut().unwrap().volumes = volumes;
        a.reconcile_disks(cx);
    });
    j.settle();
    j.read(|a| {
        assert!(a.disks.cards[&selected].snapshot.is_some());
        assert!(a.disks.cards[&displaced].snapshot.is_none());
        let model = a.home_model();
        for id in [selected, displaced] {
            let card = model.cards.iter().find(|c| c.ticket.card == id).unwrap();
            assert!(card.geometry.borrow().is_none());
            assert!(card.preview.is_none());
        }
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            j.fixture.scans[0]
        );
    });
}
#[test]
fn e1_directory_only_in_list_can_zoom_and_largest_paths_are_scoped() {
    let mut j = Journey::new(false);
    j.open(0);
    let node = j.node("a");
    j.update(|a, _, _| {
        let mut tree = a.explore.tree.as_ref().unwrap().as_ref().clone();
        tree.children.retain(|n| n.node != node);
        a.explore.tree = Some(Arc::new(tree));
    });
    j.action(Action::ZoomInto(node));
    j.settle();
    j.action(Action::ListMode(explore::ListMode::Largest));
    j.settle();
    j.read(|a| {
        assert!(
            a.explore
                .list_rows
                .iter()
                .all(|r| r.entry.kind != EntryKind::Directory)
        );
        assert!(a.explore.list_paths.iter().all(|p| p.contains("/a/")));
    });
}
#[test]
fn review_ime_preedit_survives_parent_redraw_then_commits_once() {
    use gpui::EntityInputHandler;
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let epoch = j.read(|a| a.explore.search.epoch);
    j.update(|a, w, cx| {
        let input = a.search_input.clone();
        w.focus(&input.focus_handle(cx), cx);
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "とう", Some(2..2), w, cx)
        });
        cx.notify();
    });
    j.frame();
    j.update(|a, w, cx| {
        a.search_input.update(cx, |input, cx| {
            assert_eq!(input.text(), "とう");
            assert_eq!(input.marked_text_range(w, cx), Some(0..2));
        });
        assert!(a.explore.search.text.is_empty());
    });
    j.update(|a, w, cx| {
        a.search_input
            .update(cx, |input, cx| input.unmark_text(w, cx))
    });
    j.settle();
    j.read(|a| {
        assert_eq!(a.explore.search.text, "とう");
        assert_eq!(a.explore.search.epoch, epoch + 1);
    });
    j.action(Action::SearchClear);
    j.settle();
    j.frame();
    j.update(|a, w, cx| {
        a.search_input.update(cx, |input, cx| {
            assert!(input.text().is_empty());
            assert!(input.marked_text_range(w, cx).is_none());
        })
    });
}
#[test]
fn review_direct_nested_header_preserves_ancestors_and_up_path() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let (a, b) = j.read(|s| {
        let a = s
            .explore
            .tree
            .as_ref()
            .unwrap()
            .children
            .iter()
            .find(|n| n.label == "a")
            .unwrap();
        (
            a.node,
            a.children.iter().find(|n| n.label == "b").unwrap().node,
        )
    });
    let p = j.tile_point(tree::Target::Node(b), true);
    j.visual().simulate_click(p, Default::default());
    j.settle();
    j.frame();
    j.read(|s| {
        assert_eq!(s.explore.crumbs.len(), 3);
        assert_eq!(s.explore.crumbs[1].0, a);
        assert_eq!(s.explore.crumbs[2].0, b);
    });
    j.action(Action::Up);
    j.settle();
    j.read(|s| {
        assert_eq!(s.explore.tree.as_ref().unwrap().root, a);
        assert_eq!(s.explore.selected, Some(b));
        assert_eq!(s.explore.selection_path.as_ref().map(|p| p.0), Some(b));
    });
}
#[test]
fn review_metric_totals_survive_nested_bookmarks() {
    let mut j = Journey::new(false);
    j.open(0);
    let node = j.node("a");
    j.action(Action::ZoomInto(node));
    j.settle();
    let before = j.read(|a| a.explore_model().crumbs.clone());
    j.action(Action::ToggleMetric);
    j.settle();
    let allocated = j.read(|a| {
        let m = a.explore_model();
        for (i, (_, _, bytes)) in m.crumbs.iter().enumerate() {
            assert_eq!(*bytes, a.explore.crumbs[i].2.allocated);
        }
        assert!(m.crumbs.iter().zip(&before).any(|(a, b)| a.2 != b.2));
        m.crumbs
    });
    j.open(1);
    j.open(0);
    j.read(|a| assert_eq!(a.explore_model().crumbs, allocated));
}
#[test]
fn review_long_search_caption_keeps_minimum_window_controls_visible() {
    let mut j = Journey::new(true);
    j.open(0);
    j.visual().simulate_resize(size(px(900.), px(600.)));
    j.update(|a, _, cx| {
        a.explore.search.text = "long-filename-".repeat(100);
        a.explore.search.matching = Some(0);
        a.metric = Metric::Allocated;
        let mut listing = a.volumes.clone().unwrap();
        listing.volumes[0].label = "Long disk name ".repeat(20);
        a.volumes = Some(listing);
        a.reconcile_disks(cx);
    });
    j.settle();
    j.frame();
    for selector in [
        "explore-metric",
        "explore-list-toggle",
        "explore-acquisition",
    ] {
        let b = j.visual().debug_bounds(selector).unwrap();
        assert!(
            b.origin.x >= px(0.) && b.origin.x + b.size.width <= px(900.),
            "{selector}: {b:?}"
        );
    }
}
