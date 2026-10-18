//! Remaining acceptance boundaries use the shared real scanner/controller harness.
use super::*;
use std::rc::Rc;

#[test]
fn d3_tooltip_containment_and_parent_gap_d4_enter() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    let node = j.node("a");
    let p = j.tile_point(tree::Target::Node(node), true);
    j.visual().simulate_click(p, Default::default());
    j.settle();
    j.frame();
    j.visual().simulate_keystrokes("backspace");
    j.settle();
    j.frame();
    j.read(|a| assert_eq!(a.explore.selected, Some(node)));
    j.visual().simulate_keystrokes("enter");
    j.settle();
    j.frame();
    j.read(|a| assert_eq!(a.explore.tree.as_ref().unwrap().root, node));
    let (target, p, gap) = j.read(|a| {
        let cache = a.explore.geometry.borrow();
        let g = &cache.as_ref().unwrap().geometry;
        let tile = g
            .tiles
            .iter()
            .find(|t| t.header.is_some() && !t.children.is_empty())
            .unwrap();
        let header = tile.header.unwrap();
        assert_eq!(header.height, 18.);
        // A point just inside the parent's body precedes each child's half-gap inset.
        (
            tile.target,
            point(px(tile.rect.x + 2.), px(tile.rect.y + 2.)),
            point(px(tile.body.x + 0.1), px(tile.body.y + 0.1)),
        )
    });
    let map = j.visual().debug_bounds("explore-map").unwrap();
    j.visual()
        .simulate_mouse_move(map.origin + p, None, Default::default());
    j.frame();
    j.read(|a| assert_eq!(a.explore.hovered, Some(target)));
    let tip = j.visual().debug_bounds("explore-tooltip").unwrap();
    assert!(tip.origin.x >= map.origin.x && tip.origin.y >= map.origin.y);
    assert!(tip.origin.x + tip.size.width <= map.origin.x + map.size.width);
    assert!(tip.origin.y + tip.size.height <= map.origin.y + map.size.height);
    j.visual()
        .simulate_mouse_move(map.origin + gap, None, Default::default());
    j.read(|a| assert_eq!(a.explore.hovered, Some(target)));
    j.visual().simulate_mouse_move(
        map.origin + point(px(0.1), px(0.1)),
        None,
        Default::default(),
    );
    j.read(|a| assert!(a.explore.hovered.is_none()));
}

#[test]
fn e3_duplicate_hover_has_no_notification_and_j4_stale_hover() {
    let mut j = Journey::new(false);
    j.open(0);
    let target = tree::Target::Node(j.node("a"));
    let notifications = Rc::new(std::cell::Cell::new(0));
    let count = notifications.clone();
    let entity = j.window.entity(&j.cx).unwrap();
    let _subscription =
        j.cx.update(|cx| cx.observe(&entity, move |_, _| count.set(count.get() + 1)));
    j.action(Action::Hover(Some(target)));
    j.cx.run_until_parked();
    let first = notifications.get();
    assert!(first > 0);
    j.action(Action::Hover(Some(target)));
    j.cx.run_until_parked();
    assert_eq!(notifications.get(), first);
    let stale = j.read(|a| a.explore_model().event(Action::Hover(Some(target))));
    j.open(1);
    j.update(|a, w, cx| a.explore_event(stale, w, cx));
    j.read(|a| assert!(a.explore.hovered.is_none()));
    j.update(|a, w, cx| {
        let event = a
            .explore_model()
            .event(Action::Select(Some(NodeId::new(99_999_999).unwrap())));
        assert!(!a.explore_model().allows(&event));
        a.explore_event(event, w, cx);
        assert!(a.explore.selected.is_none());
    });
}

#[test]
fn h2_stale_home_ticket_h6_duplicate_lazy_open() {
    let mut j = Journey::new(false);
    let ticket = j.ticket(0);
    j.update(|a, w, cx| {
        let mut stale = ticket;
        stale.query_epoch += 1;
        assert!(!a.home_model().allows(home::Event::Explore(stale)));
        a.home_event(home::Event::Explore(stale), w, cx);
        assert_eq!(a.screen, Screen::Home);
        a.disks.cards.get_mut(&ticket.card).unwrap().snapshot = None;
        a.home_event(home::Event::Explore(ticket), w, cx);
        assert!(a.disks.cards[&ticket.card].opening);
        let request = a.disks.open_request;
        assert!(!a.home_model().allows(home::Event::Explore(ticket)));
        a.home_event(home::Event::Explore(ticket), w, cx);
        assert_eq!(a.disks.open_request, request);
    });
    j.settle();
    j.read(|a| {
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            ticket.scan.unwrap()
        )
    });
}

fn file_names(nodes: &[tree::TreeNode], out: &mut Vec<String>) {
    for node in nodes {
        if node.is_directory {
            file_names(&node.children, out);
        } else {
            out.push(node.label.clone());
        }
    }
}
#[test]
fn j6_every_filtered_leaf_and_row_matches_and_clear_restores_tree() {
    let mut j = Journey::new(false);
    j.open(0);
    let original = j.read(|a| format!("{:?}", a.explore.tree.as_ref().unwrap()));
    j.update(|a, _, cx| a.search_changed("alpha-001".into(), cx));
    j.settle();
    j.read(|a| {
        let mut names = vec![];
        file_names(&a.explore.tree.as_ref().unwrap().children, &mut names);
        assert!(!names.is_empty());
        assert!(names.iter().all(|n| n.contains("alpha-001")));
        assert!(
            a.explore
                .list_rows
                .iter()
                .filter(|r| r.entry.kind != EntryKind::Directory)
                .all(|r| r.entry.display_name.contains("alpha-001"))
        );
    });
    j.action(Action::ListMode(explore::ListMode::Largest));
    j.settle();
    j.read(|a| {
        assert!(!a.explore.list_rows.is_empty());
        assert!(
            a.explore
                .list_rows
                .iter()
                .all(|r| r.entry.display_name.contains("alpha-001"))
        );
    });
    j.action(Action::SearchClear);
    j.settle();
    j.read(|a| assert_eq!(format!("{:?}", a.explore.tree.as_ref().unwrap()), original));
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
        assert!(!a.explore.list_paths.is_empty());
        let scope = j.fixture.directory.join("alpha").join("a");
        assert!(
            a.explore
                .list_paths
                .iter()
                .zip(&a.explore.list_rows)
                .all(|(p, row)| {
                    let relative = std::path::Path::new(p);
                    relative.is_relative()
                        && relative
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                        && scope.join(relative).is_file()
                        && relative.file_name().unwrap().to_string_lossy() == row.entry.display_name
                })
        );
    });
}

#[test]
fn d6_disk_row_overlap_selects_only_disk() {
    let mut j = Journey::new(true);
    j.open(0);
    j.frame();
    j.action(Action::ToggleDiskMenu);
    j.settle();
    j.frame();
    let disk = j.read(|a| {
        a.explore_model()
            .disks
            .iter()
            .position(|d| d.ticket.scan == Some(j.fixture.scans[1]))
            .unwrap()
    });
    let row = j
        .visual()
        .debug_bounds(if disk == 0 {
            "disk-choice-0"
        } else {
            "disk-choice-1"
        })
        .unwrap();
    let map = j.visual().debug_bounds("explore-map").unwrap();
    let p = row.origin + point(row.size.width / 2., row.size.height / 2.);
    assert!(p.y >= map.origin.y && p.x >= map.origin.x);
    j.visual().simulate_mouse_move(p, None, Default::default());
    j.read(|a| assert!(a.explore.hovered.is_none()));
    j.visual().simulate_click(p, Default::default());
    j.settle();
    j.frame();
    j.read(|a| {
        assert_eq!(
            a.explore.snapshot.as_ref().unwrap().scan,
            j.fixture.scans[1]
        );
        assert!(a.explore.selected.is_none());
        assert_eq!(a.explore.crumbs.len(), 1);
        assert!(!a.explore.disk_menu_open);
    });
}

#[test]
fn home_refresh_fits_hero_above_cards_at_both_widths() {
    let mut j = Journey::new(true);
    for (width, height) in [(1280., 800.), (900., 600.)] {
        j.visual().simulate_resize(size(px(width), px(height)));
        j.settle();
        j.frame();
        let hero = j.visual().debug_bounds("home-hero").unwrap();
        let refresh = j.visual().debug_bounds("home-refresh").unwrap();
        let first = j.visual().debug_bounds("first-disk-card").unwrap();
        assert_eq!(hero.size.height, px(112.));
        assert!(refresh.origin.y >= hero.origin.y + px(12.));
        assert!(
            refresh.origin.y + refresh.size.height <= hero.origin.y + hero.size.height - px(12.)
        );
        assert!(refresh.origin.y + refresh.size.height <= first.origin.y);
        assert!(refresh.origin.x + refresh.size.width <= hero.origin.x + hero.size.width);
    }
}
