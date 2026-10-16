//! Focused explorer content composed into the dashboard's presentation shell.
use super::*;
impl Space {
    pub(super) fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let size = window.viewport_size();
        let width = f32::from(size.width);
        let height = f32::from(size.height);
        if self.disks.home {
            let model = self.dashboard_model();
            if self.cache_visible {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .child(control("cache-home", "All disks").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.cache_visible = false;
                            cx.notify();
                        },
                    )))
                    .child(
                        div()
                            .id("cache-home-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(cache_management::render(
                                &self.cache_model,
                                cx,
                                Self::cache_event,
                            )),
                    );
            }
            return dashboard::view(&model, width, height, cx, Self::dashboard_event);
        }
        let model = self.explorer_model();
        let content = self.explorer_content(width, height, cx).into_any_element();
        let inspector = self
            .disks
            .inspector_visible
            .then(|| self.render_inspector(cx).into_any_element());
        explorer::render(
            &model,
            content,
            inspector,
            width,
            height,
            cx,
            Self::explorer_event,
        )
    }
    fn explorer_content(&mut self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        if self.changes.visible {
            return div().size_full().child(
                div()
                    .id("changes-surface")
                    .size_full()
                    .overflow_y_scroll()
                    .child(changes_panel::render(
                        &self.changes.model,
                        cx,
                        Self::changes_event,
                    )),
            );
        }
        if self.cache_visible {
            return div().size_full().child(
                div()
                    .id("cache-surface")
                    .size_full()
                    .overflow_y_scroll()
                    .child(cache_management::render(
                        &self.cache_model,
                        cx,
                        Self::cache_event,
                    )),
            );
        }
        let mut content = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3();
        if let Some(notice) = &self.disks.explorer_notice {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa7b9c8))
                    .child(notice.clone()),
            );
        }
        if self.filters_visible {
            content = content.child(self.render_filters(cx));
        }
        if let Some(error) = &self.filter_error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xf2cf91))
                    .child(error.clone()),
            );
        }
        if self.filter_busy {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.filter_status.clone())
                    .child(control("filter-stop", "Stop filtering").on_click(
                        cx.listener(|this, _, w, cx| this.cancel_filter(&CancelFilter, w, cx)),
                    )),
            );
        }
        let show_list = self.view != View::Treemap || self.disks.list_visible;
        if self.view == View::Treemap {
            let rendered = (
                self.snapshot.as_ref().map(|s| s.scan),
                self.query_generation,
            );
            let map = treemap::view_sized(
                &self.treemap,
                self.chart_selected,
                treemap::Size::Fill,
                cx,
                move |this: &mut Self, event, w, cx| {
                    if rendered
                        == (
                            this.snapshot.as_ref().map(|s| s.scan),
                            this.query_generation,
                        )
                    {
                        this.chart_event(event, w, cx);
                    }
                },
            );
            // A finite available-height parent is essential: relative map tiles fill this area.
            let map_height = (explorer::layout(width, height, self.disks.inspector_visible)
                .map_height
                - if show_list { 200. } else { 0. })
            .max(180.);
            content = content.child(
                div()
                    .h(px(map_height))
                    .min_h(px(180.))
                    .flex_1()
                    .min_w_0()
                    .child(map),
            );
        }
        if show_list {
            let list = self.render_entry_list(cx);
            content = content.child(if self.view == View::Treemap {
                div().h(px(200.)).min_h_0().child(list)
            } else {
                div().flex_1().min_h_0().child(list)
            });
        }
        content = content.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa7b9c8))
                        .child(if self.query_busy {
                            "Loading cached page…"
                        } else if self.rows.is_empty() {
                            "No included entries in this view."
                        } else {
                            "Cached metadata · no source file contents opened"
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(control("page-first", "First page").on_click(
                            cx.listener(|this, _, w, cx| this.first_page(&FirstPage, w, cx)),
                        ))
                        .child(
                            dashboard::button(
                                "page-next",
                                "Next page",
                                self.next_page.is_some(),
                                false,
                            )
                            .on_click(
                                cx.listener(|this, _, w, cx| this.next_page(&NextPage, w, cx)),
                            ),
                        ),
                ),
        );
        div()
            .size_full()
            .min_h_0()
            .child(content.id("explorer-body-scroll").overflow_y_scroll())
    }
    fn render_filters(&mut self, cx: &mut Context<Self>) -> Div {
        let inputs: Vec<_> = self
            .filter_inputs
            .iter()
            .cloned()
            .enumerate()
            .filter(|(index, _)| *index == 0 || self.disks.advanced_filters)
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(rgb(0x20313e))
            .child(
                div()
                    .id("filter-fields")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .children(inputs.into_iter().map(|(index, input)| {
                        div().id(("filter-input", index)).w(px(220.)).child(input)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(control("filter-apply", "Apply").on_click(
                        cx.listener(|this, _, w, cx| this.apply_filters(&ApplyFilters, w, cx)),
                    ))
                    .child(control("filter-reset", "Reset").on_click(
                        cx.listener(|this, _, w, cx| this.reset_filters(&ResetFilters, w, cx)),
                    ))
                    .child(
                        control("filter-advanced", "Advanced filters").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.disks.advanced_filters = !this.disks.advanced_filters;
                                cx.notify();
                            },
                        )),
                    )
                    .when(self.disks.advanced_filters, |element| {
                        element
                            .child(
                                control("filter-unknown-size", "Include unknown size").on_click(
                                    cx.listener(|this, _, w, cx| {
                                        this.unknown_size(&ToggleUnknownSize, w, cx)
                                    }),
                                ),
                            )
                            .child(
                                control("filter-unknown-time", "Include unknown time").on_click(
                                    cx.listener(|this, _, w, cx| {
                                        this.unknown_time(&ToggleUnknownTime, w, cx)
                                    }),
                                ),
                            )
                    }),
            )
            .child(div().text_sm().text_color(rgb(0xa7b9c8)).child(format!(
                "{} · unknown sizes {} · unknown times {}",
                self.filter_status, self.draft_unknown_size, self.draft_unknown_time
            )))
    }
    fn render_entry_list(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("entries")
            .role(Role::ListBox)
            .aria_label("Cached entries")
            .track_focus(&self.list_focus)
            .tab_stop(true)
            .key_context("SpaceList")
            .size_full()
            .min_h_0()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x304352))
            .focus(|style| style.border_color(rgb(0x90d5ff)))
            .on_action(cx.listener(|this, _: &NextRow, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousRow, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(Self::open_row))
            .child(
                uniform_list(
                    "rows",
                    self.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let row = &this.rows[index];
                                let node = row.entry.node;
                                let scan = this.snapshot.as_ref().map(|s| s.scan);
                                let generation = this.query_generation;
                                let label = format!(
                                    "{} {} · {}{}",
                                    if row.entry.kind == EntryKind::Directory {
                                        "▸"
                                    } else {
                                        "·"
                                    },
                                    row.entry.display_name,
                                    total_text(row, this.applied_metric),
                                    if !row.canonical {
                                        " · hardlink alias"
                                    } else {
                                        ""
                                    }
                                );
                                div()
                                    .id(index)
                                    .role(Role::ListBoxOption)
                                    .aria_label(SharedString::from(label.clone()))
                                    .aria_selected(index == this.selected)
                                    .aria_position_in_set(index + 1)
                                    .aria_size_of_set(this.rows.len())
                                    .h(px(34.))
                                    .px_3()
                                    .cursor_pointer()
                                    .when(index == this.selected, |element| {
                                        element.bg(rgb(0x31566c)).aria_active_descendant()
                                    })
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, w, cx| {
                                            if generation != this.query_generation
                                                || scan != this.snapshot.as_ref().map(|s| s.scan)
                                                || this
                                                    .rows
                                                    .get(index)
                                                    .is_none_or(|row| row.entry.node != node)
                                            {
                                                return;
                                            }
                                            w.focus(&this.list_focus, cx);
                                            this.clear_aliases();
                                            this.selected = index;
                                            this.chart_selected = Some(node);
                                            if event.click_count() >= 2 {
                                                this.open_row(&OpenRow, w, cx);
                                            }
                                            cx.notify();
                                        },
                                    ))
                                    .child(label)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .h_full(),
            )
    }
    fn render_inspector(&mut self, cx: &mut Context<Self>) -> Div {
        let selected = self
            .chart_selected
            .and_then(|node| self.rows.iter().find(|row| row.entry.node == node))
            .or_else(|| self.rows.get(self.selected));
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_3()
            .min_w_0()
            .child(div().text_lg().child("Item details"));
        if let Some(row) = selected {
            panel = panel
                .child(row.entry.display_name.clone())
                .child(total_text(row, self.applied_metric))
                .child(div().text_sm().text_color(rgb(0xa7b9c8)).child(format!(
                    "{} · {}",
                    if row.entry.kind == EntryKind::Directory {
                        "Folder"
                    } else {
                        "File"
                    },
                    if row.canonical {
                        "Charged canonical entry"
                    } else {
                        "Hardlink alias; charged elsewhere"
                    }
                )))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            control("detail-copy", "Copy path").on_click(
                                cx.listener(|this, _, _, cx| this.path_action(false, cx)),
                            ),
                        )
                        .child(
                            control("detail-reveal", "Reveal")
                                .on_click(cx.listener(|this, _, _, cx| this.path_action(true, cx))),
                        )
                        .child(control("detail-aliases", "Hardlink aliases").on_click(
                            cx.listener(|this, _, w, cx| this.show_aliases(&ShowAliases, w, cx)),
                        )),
                );
        } else {
            panel = panel.child("Select a tile or list entry to inspect it.");
        }
        if !self.alias_nodes.is_empty() {
            panel = panel
                .child(
                    div()
                        .id("alias-list")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .children(self.alias_nodes.iter().map(|(node, path)| {
                            let node = *node;
                            let generation = self.alias_generation;
                            let scan = self.snapshot.as_ref().map(|s| s.scan);
                            let label = format!(
                                "{} {}",
                                if self.alias_selected == Some(node) {
                                    "Selected:"
                                } else {
                                    ""
                                },
                                path
                            );
                            dashboard::button(("alias", node.get() as usize), label, true, false)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if alias_selection_is_current(
                                        (generation, scan),
                                        (
                                            this.alias_generation,
                                            this.snapshot.as_ref().map(|s| s.scan),
                                        ),
                                        node,
                                        &this.alias_nodes,
                                    ) {
                                        this.alias_selected = Some(node);
                                        cx.notify();
                                    }
                                }))
                        })),
                )
                .child(control("aliases-next", "Next aliases").on_click(
                    cx.listener(|this, _, w, cx| this.next_aliases(&NextAliases, w, cx)),
                ));
        }
        panel.child(div().text_sm().text_color(rgb(0xa7b9c8)).child(self.status.clone())).child(div().text_sm().child("Sizes are observed included bytes, not reclaimable space. Reveal opens the current path only."))
    }
}
