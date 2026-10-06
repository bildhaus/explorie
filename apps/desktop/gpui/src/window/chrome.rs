//! `DirectoryWindow` behavior for chrome.

use crate::*;

impl DirectoryWindow {
    #[cfg(target_os = "windows")]
    pub(crate) fn render_title_bar(
        &mut self,
        maximized: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette;
        let titlebar_height = 36.0 * palette.scale;
        let control =
            |id: &'static str, label: &'static str, icon: &'static str, area: WindowControlArea| {
                div()
                    .id(id)
                    .debug_selector(move || id.to_string())
                    .role(Role::Button)
                    .aria_label(label)
                    .window_control_area(area)
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(44.0 * palette.scale))
                    .h_full()
                    .flex_none()
                    .text_color(palette.muted)
                    .child(toolbar_icon(
                        icon,
                        palette.icon_size.clamp(13.0, 18.0),
                        palette.muted,
                    ))
            };
        let maximize_label = if maximized {
            "Restore window"
        } else {
            "Maximize window"
        };

        div()
            .id("title-bar")
            .debug_selector(|| "title-bar".to_string())
            .flex()
            .items_center()
            .w_full()
            .h(px(titlebar_height))
            .flex_none()
            .bg(palette.topbar)
            .text_color(palette.text)
            .child(
                div()
                    .id("title-bar-drag-region")
                    .debug_selector(|| "title-bar-drag-region".to_string())
                    .window_control_area(WindowControlArea::Drag)
                    .flex()
                    .flex_1()
                    .items_center()
                    .h_full()
                    .min_w_0()
                    .pl_2()
                    .gap_1()
                    .child(
                        div()
                            .id("title-bar-app-icon")
                            .debug_selector(|| "title-bar-app-icon".to_string())
                            .w(px(22.0))
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                img("icons/titlebar-icon.png")
                                    .w(px(22.0))
                                    .h(px(22.0))
                                    .object_fit(ObjectFit::Contain),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(APP_NAME),
                    ),
            )
            .child(
                control(
                    "window-minimize",
                    "Minimize window",
                    "minus",
                    WindowControlArea::Min,
                )
                .hover(move |button| button.bg(palette.hover).text_color(palette.text))
                .on_click(|_, window, _| window.minimize_window()),
            )
            .child(
                control(
                    "window-maximize",
                    maximize_label,
                    "frame",
                    WindowControlArea::Max,
                )
                .hover(move |button| button.bg(palette.hover).text_color(palette.text))
                .on_click(|_, window, _| window.zoom_window()),
            )
            .child(
                control(
                    "window-close",
                    "Close window",
                    "close",
                    WindowControlArea::Close,
                )
                .hover(|button| button.bg(rgb(0xff8aa0)).text_color(rgb(0xffffff)))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.request_window_close(cx) {
                        window.remove_window();
                    }
                })),
            )
            .into_any_element()
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn render_title_bar(&mut self, _: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let titlebar_height = 36.0 * palette.scale;
        let traffic_light_padding = 78.0 * palette.scale;
        let folder_name = path_label(self.browser.path());

        div()
            .id("title-bar")
            .debug_selector(|| "title-bar".to_string())
            .flex()
            .items_center()
            .w_full()
            .h(px(titlebar_height))
            .flex_none()
            .bg(palette.topbar)
            .text_color(palette.text)
            .child(
                div()
                    .id("title-bar-drag-region")
                    .debug_selector(|| "title-bar-drag-region".to_string())
                    .window_control_area(WindowControlArea::Drag)
                    .flex()
                    .items_center()
                    .w_full()
                    .h_full()
                    .on_mouse_down_out(cx.listener(|this, _, _, _| {
                        this.title_bar_drag_pending = false;
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| {
                            this.title_bar_drag_pending = false;
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| {
                            this.title_bar_drag_pending = true;
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, _, window, _| {
                        if this.title_bar_drag_pending {
                            this.title_bar_drag_pending = false;
                            window.start_window_move();
                        }
                    }))
                    .on_click(|event, window, _| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    })
                    .child(div().w(px(traffic_light_padding)).h_full().flex_none())
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .min_w_0()
                            .child(
                                div()
                                    .id("title-bar-folder-name")
                                    .debug_selector(|| "title-bar-folder-name".to_string())
                                    .aria_label(format!("Current folder: {folder_name}"))
                                    .max_w(px(360.0 * palette.scale))
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(folder_name),
                            ),
                    )
                    .child(div().w(px(traffic_light_padding)).h_full().flex_none()),
            )
            .into_any_element()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub(crate) fn render_title_bar(&mut self, _: bool, _: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }

    pub(crate) fn render_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let tab_height = if palette.density == Density::Compact {
            32.0
        } else {
            36.0
        } * palette.scale;
        let active = self.browser.active_tab_id();
        let tab_count = self.browser.tabs().len();
        let tabs: Vec<_> = self
            .browser
            .tabs()
            .iter()
            .map(|tab| (tab.id(), tab.path().to_path_buf()))
            .collect();
        let mut tab_elements = Vec::with_capacity(tab_count + 1);

        for (id, path) in tabs {
            let selected = id == active;
            let label = path_label(&path);
            let drag_style_path = path.clone();
            let drop_path = path.clone();
            let external_drop_path = path.clone();
            let leave_target = FileDragHoverTarget::Tab(id);
            let palette = self.palette;
            tab_elements.push(
                div()
                    .id(("tab", id.value()))
                    .debug_selector(move || format!("tab-{}", id.value()))
                    .role(Role::Tab)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .focusable()
                    .tab_stop(true)
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w(px(96.0))
                    .max_w(px(220.0))
                    .px_2()
                    .py_1()
                    .rounded(px(palette.radius))
                    .bg(if selected {
                        self.palette.window
                    } else {
                        with_alpha(self.palette.panel, 0.0)
                    })
                    .text_color(if selected {
                        self.palette.text
                    } else {
                        self.palette.muted
                    })
                    .hover(move |tab| tab.bg(palette.hover).text_color(palette.text))
                    .focus(move |tab| tab.bg(palette.selected).text_color(palette.text))
                    .cursor_pointer()
                    .child(toolbar_icon(
                        "folder",
                        palette.icon_size.clamp(13.0, 18.0),
                        if selected {
                            palette.accent
                        } else {
                            palette.muted
                        },
                    ))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
                    .when(tab_count > 1, |tab| {
                        tab.child(
                            div()
                                .id(("close-tab", id.value()))
                                .role(Role::Button)
                                .aria_label("Close tab")
                                .focusable()
                                .tab_stop(true)
                                .px_1()
                                .rounded(px(palette.radius))
                                .focus(move |button| button.bg(palette.hover))
                                .hover(move |button| button.bg(palette.hover))
                                .child(toolbar_icon(
                                    "close",
                                    palette.icon_size.clamp(11.0, 15.0),
                                    palette.muted,
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_tab(id, cx);
                                })),
                        )
                    })
                    .drag_over::<FileDrag>(move |style, drag, window, _| {
                        if valid_file_drop_target(
                            &drag_style_path,
                            drag,
                            file_drag_operation(window),
                        ) {
                            style.border_color(palette.accent).bg(palette.selected)
                        } else {
                            style
                        }
                    })
                    .on_drag_move::<FileDrag>(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<FileDrag>, _, cx| {
                            let drag = event.drag(cx).clone();
                            this.hover_file_drag_tab(&drag, id, cx);
                        },
                    ))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                        if !*hovered {
                            this.cancel_file_drag_hover(&leave_target);
                        }
                    }))
                    .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                        cx.stop_propagation();
                        this.drop_files_to(drag, drop_path.clone(), false, window, cx);
                    }))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(palette.accent).bg(palette.selected)
                    })
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                        cx.stop_propagation();
                        this.drop_external_paths_to(paths, external_drop_path.clone(), cx);
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_tab(id, cx);
                    }))
                    .into_any_element(),
            );
        }

        tab_elements.push(
            div()
                .id("new-tab")
                .role(Role::Button)
                .aria_label("New tab")
                .focusable()
                .tab_stop(true)
                .px_3()
                .py_1()
                .rounded(px(palette.radius))
                .focus(move |button| button.bg(palette.hover))
                .hover(move |button| button.bg(palette.hover))
                .cursor_pointer()
                .child(toolbar_icon(
                    "plus",
                    palette.icon_size.clamp(13.0, 18.0),
                    palette.muted,
                ))
                .on_click(cx.listener(|this, _, _, cx| this.new_tab(cx)))
                .into_any_element(),
        );
        div()
            .id("tabs")
            .debug_selector(|| "tabs".to_string())
            .role(Role::TabList)
            .aria_label("Folder tabs")
            .flex()
            .w_full()
            .h(px(tab_height))
            .items_center()
            .gap_1()
            .p_1()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(self.palette.border)
            .bg(self.palette.surface)
            .children(tab_elements)
            .into_any_element()
    }

    pub(crate) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        {
            self.render_stats.sidebar += 1;
        }
        if self.layout.sidebar_collapsed {
            return div().id("sidebar-collapsed").w_0().into_any_element();
        }
        let palette = self.palette;
        let current_path = self.browser.path().to_path_buf();
        let favorites: Vec<_> = self
            .browser
            .favorites()
            .iter()
            .map(|favorite| (favorite.path().to_path_buf(), favorite.name().to_string()))
            .collect();
        self.favorite_focus_handles.truncate(favorites.len());
        while self.favorite_focus_handles.len() < favorites.len() {
            let tab_index = self.favorite_focus_handles.len() as isize + 1;
            self.favorite_focus_handles
                .push(cx.focus_handle().tab_index(tab_index).tab_stop(true));
        }
        let recents = self.browser.recents().to_vec();
        let mut favorite_rows = Vec::with_capacity(favorites.len().max(1));
        if favorites.is_empty() {
            favorite_rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(palette.tertiary)
                    .child("No favorites yet")
                    .into_any_element(),
            );
        } else {
            for (index, (path, name)) in favorites.into_iter().enumerate() {
                let navigation_path = path.clone();
                let drag = FavoriteDrag::new(path.clone(), name.clone());
                let focus_handle = self.favorite_focus_handles[index].clone();
                let click_focus_handle = focus_handle.clone();
                let drop_target = index;
                let drag_palette = self.palette;
                let file_drag_style_path = path.clone();
                let file_drop_path = path.clone();
                let external_file_drop_path = path.clone();
                let file_hover_path = path.clone();
                let file_leave_target = FileDragHoverTarget::Folder(path.clone());
                favorite_rows.push(
                    div()
                        .id(("favorite", index))
                        .debug_selector(move || format!("favorite-{index}"))
                        .group("favorite-row")
                        .key_context("favorite")
                        .track_focus(&focus_handle)
                        .tab_stop(true)
                        .role(Role::Button)
                        .aria_label(name.clone())
                        .flex()
                        .items_center()
                        .gap_1()
                        .mx_2()
                        .px_2()
                        .py_1()
                        .rounded(px(palette.radius))
                        .border_1()
                        .border_color(with_alpha(self.palette.border, 0.0))
                        .focus(move |row| row.border_color(palette.accent).bg(palette.selected))
                        .hover(move |row| row.bg(palette.hover))
                        .cursor_pointer()
                        .child(toolbar_icon("bookmark", 14.0, palette.muted))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                        .child(
                            div()
                                .id(("remove-favorite", index))
                                .role(Role::Button)
                                .aria_label("Remove favorite")
                                .focusable()
                                .tab_stop(true)
                                .invisible()
                                .px_1()
                                .rounded(px(palette.radius))
                                .group_hover("favorite-row", |button| button.visible())
                                .focus(move |button| button.visible().bg(palette.hover))
                                .hover(move |button| button.visible().bg(palette.hover))
                                .cursor_pointer()
                                .child(toolbar_icon(
                                    "close",
                                    palette.icon_size.clamp(11.0, 15.0),
                                    palette.muted,
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mutate_shared_session(|session| {
                                        session.remove_favorite(&path)
                                    });
                                    this.persist_session();
                                    cx.stop_propagation();
                                    cx.notify();
                                })),
                        )
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            window.focus(&click_focus_handle, cx);
                        })
                        .on_action(cx.listener(move |this, _: &MoveFavoriteUp, window, cx| {
                            this.move_favorite_at(index, -1, window, cx);
                        }))
                        .on_action(cx.listener(move |this, _: &MoveFavoriteDown, window, cx| {
                            this.move_favorite_at(index, 1, window, cx);
                        }))
                        .on_drag(drag, |drag, position, _, cx| {
                            cx.new(|_| drag.clone().at(position))
                        })
                        .drag_over::<FileDrag>(move |style, drag, window, _| {
                            if valid_file_drop_target(
                                &file_drag_style_path,
                                drag,
                                file_drag_operation(window),
                            ) {
                                style
                                    .border_color(drag_palette.accent)
                                    .bg(drag_palette.selected)
                            } else {
                                style
                            }
                        })
                        .on_drag_move::<FileDrag>(cx.listener(
                            move |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                                let drag = event.drag(cx).clone();
                                this.hover_file_drag_folder(
                                    &drag,
                                    file_hover_path.clone(),
                                    false,
                                    window,
                                    cx,
                                );
                            },
                        ))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                            if !*hovered {
                                this.cancel_file_drag_hover(&file_leave_target);
                            }
                        }))
                        .drag_over::<FavoriteDrag>(move |style, _, _, _| {
                            style
                                .border_color(drag_palette.accent)
                                .bg(drag_palette.selected)
                        })
                        .on_drop(cx.listener(move |this, drag: &FavoriteDrag, window, cx| {
                            this.reorder_favorite_to(&drag.path, drop_target, window, cx);
                        }))
                        .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                            cx.stop_propagation();
                            this.drop_files_to(drag, file_drop_path.clone(), false, window, cx);
                        }))
                        .drag_over::<ExternalPaths>(move |style, _, _, _| {
                            style
                                .border_color(drag_palette.accent)
                                .bg(drag_palette.selected)
                        })
                        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                            cx.stop_propagation();
                            this.drop_external_paths_to(paths, external_file_drop_path.clone(), cx);
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate_to(navigation_path.clone(), cx);
                        }))
                        .into_any_element(),
                );
            }
        }

        let mut recent_rows = Vec::with_capacity(recents.len().max(1));
        if recents.is_empty() {
            recent_rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(palette.tertiary)
                    .child("No recent locations")
                    .into_any_element(),
            );
        } else {
            for (index, path) in recents.into_iter().enumerate() {
                let label = path_label(&path);
                let click_path = path.clone();
                let drag_style_path = path.clone();
                let hover_path = path.clone();
                let drop_path = path.clone();
                let external_drop_path = path.clone();
                let leave_target = FileDragHoverTarget::Folder(path);
                let palette = self.palette;
                recent_rows.push(
                    div()
                        .id(("recent", index))
                        .debug_selector(move || format!("recent-{index}"))
                        .role(Role::Button)
                        .aria_label(label.clone())
                        .focusable()
                        .tab_stop(true)
                        .flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .border_1()
                        .border_color(with_alpha(self.palette.border, 0.0))
                        .focus(move |row| row.border_color(palette.accent).bg(palette.selected))
                        .px_2()
                        .py_1()
                        .rounded(px(palette.radius))
                        .truncate()
                        .text_sm()
                        .hover(move |row| row.bg(palette.hover))
                        .cursor_pointer()
                        .child(toolbar_icon("folder", 14.0, palette.muted))
                        .child(div().min_w_0().truncate().child(label))
                        .drag_over::<FileDrag>(move |style, drag, window, _| {
                            if valid_file_drop_target(
                                &drag_style_path,
                                drag,
                                file_drag_operation(window),
                            ) {
                                style.border_color(palette.accent).bg(palette.selected)
                            } else {
                                style
                            }
                        })
                        .on_drag_move::<FileDrag>(cx.listener(
                            move |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                                let drag = event.drag(cx).clone();
                                this.hover_file_drag_folder(
                                    &drag,
                                    hover_path.clone(),
                                    false,
                                    window,
                                    cx,
                                );
                            },
                        ))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                            if !*hovered {
                                this.cancel_file_drag_hover(&leave_target);
                            }
                        }))
                        .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                            cx.stop_propagation();
                            this.drop_files_to(drag, drop_path.clone(), false, window, cx);
                        }))
                        .drag_over::<ExternalPaths>(move |style, _, _, _| {
                            style.border_color(palette.accent).bg(palette.selected)
                        })
                        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                            cx.stop_propagation();
                            this.drop_external_paths_to(paths, external_drop_path.clone(), cx);
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate_to(click_path.clone(), cx);
                        }))
                        .into_any_element(),
                );
            }
        }

        let favorites_drop_palette = self.palette;
        let favorites_drop_target = div()
            .id("favorites-file-drop-target")
            .debug_selector(|| "favorites-file-drop-target".to_string())
            .child(sidebar_section_label("Favorites", palette))
            .children(favorite_rows)
            .drag_over::<FileDrag>(move |style, drag, _, _| {
                if drag.items().iter().any(|item| item.is_dir) {
                    style
                        .border_color(favorites_drop_palette.accent)
                        .bg(with_alpha(favorites_drop_palette.accent, 0.08))
                } else {
                    style
                }
            })
            .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                if super::drag_drop::external_paths_include_directory(paths) {
                    style
                        .border_color(favorites_drop_palette.accent)
                        .bg(with_alpha(favorites_drop_palette.accent, 0.08))
                } else {
                    style
                }
            })
            .on_drag_move::<FileDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                    let drag = event.drag(cx).clone();
                    this.track_file_drag(&drag, cx);
                    this.maybe_start_external_file_drag(&drag, event.event.position, window, cx);
                },
            ))
            .on_drop(cx.listener(|this, drag: &FileDrag, _, cx| {
                this.drop_files_to_favorites(drag, cx);
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                cx.stop_propagation();
                this.drop_external_paths_to_favorites(paths, cx);
            }));
        let favorites_section = div()
            .id("favorites-section")
            .debug_selector(|| "favorites-section".to_string())
            .child(favorites_drop_target);

        let mut location_rows = Vec::new();
        if let Some(locations) = &self.system.locations {
            let mut locations_with_labels = Vec::new();
            for (label, path, icon) in [
                ("Home", locations.home.as_ref(), "home"),
                ("Desktop", locations.desktop.as_ref(), "desktop"),
                ("Documents", locations.documents.as_ref(), "file"),
                ("Downloads", locations.downloads.as_ref(), "download"),
                ("Music", locations.music.as_ref(), "music"),
                ("Pictures", locations.pictures.as_ref(), "image"),
                ("Videos", locations.videos.as_ref(), "video"),
            ] {
                if let Some(path) = path {
                    locations_with_labels.push((label.to_string(), PathBuf::from(path), icon));
                }
            }
            if locations.volumes.is_empty() {
                for drive in &locations.drives {
                    locations_with_labels.push((drive.clone(), PathBuf::from(drive), "drive"));
                }
            }
            for volume in &locations.volumes {
                locations_with_labels.push((
                    volume_label(volume),
                    PathBuf::from(&volume.path),
                    "drive",
                ));
            }
            let mut seen = std::collections::HashSet::new();
            locations_with_labels.retain(|(_, path, _)| seen.insert(path.clone()));
            let active_location =
                most_specific_location_index(&current_path, &locations_with_labels);
            for (index, (label, path, icon)) in locations_with_labels.into_iter().enumerate() {
                let click_path = path.clone();
                let drag_style_path = path.clone();
                let hover_path = path.clone();
                let drop_path = path.clone();
                let external_drop_path = path.clone();
                let leave_target = FileDragHoverTarget::Folder(path);
                let palette = self.palette;
                let active = active_location == Some(index);
                location_rows.push(
                    div()
                        .id(("location", index))
                        .debug_selector(move || format!("location-{index}"))
                        .role(Role::Button)
                        .aria_label(label.clone())
                        .aria_selected(active)
                        .focusable()
                        .tab_stop(true)
                        .flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .truncate()
                        .text_sm()
                        .bg(if active {
                            palette.selected
                        } else {
                            palette.panel
                        })
                        .hover(move |row| row.bg(palette.hover))
                        .focus(move |row| row.bg(palette.selected))
                        .cursor_pointer()
                        .child(toolbar_icon(icon, 14.0, palette.muted))
                        .child(div().min_w_0().truncate().child(label))
                        .drag_over::<FileDrag>(move |style, drag, window, _| {
                            if valid_file_drop_target(
                                &drag_style_path,
                                drag,
                                file_drag_operation(window),
                            ) {
                                style.border_color(palette.accent).bg(palette.selected)
                            } else {
                                style
                            }
                        })
                        .on_drag_move::<FileDrag>(cx.listener(
                            move |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                                let drag = event.drag(cx).clone();
                                this.hover_file_drag_folder(
                                    &drag,
                                    hover_path.clone(),
                                    false,
                                    window,
                                    cx,
                                );
                            },
                        ))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                            if !*hovered {
                                this.cancel_file_drag_hover(&leave_target);
                            }
                        }))
                        .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                            cx.stop_propagation();
                            this.drop_files_to(drag, drop_path.clone(), false, window, cx);
                        }))
                        .drag_over::<ExternalPaths>(move |style, _, _, _| {
                            style.border_color(palette.accent).bg(palette.selected)
                        })
                        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                            cx.stop_propagation();
                            this.drop_external_paths_to(paths, external_drop_path.clone(), cx);
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate_to(click_path.clone(), cx);
                        }))
                        .into_any_element(),
                );
            }
        } else if let Some(error) = &self.system.locations_error {
            location_rows.push(
                div()
                    .id("retry-locations")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0xffb86c))
                    .cursor_pointer()
                    .child(format!("Locations unavailable — click to retry: {error}"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_system_locations(cx);
                    }))
                    .into_any_element(),
            );
        } else {
            location_rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(palette.tertiary)
                    .child("Loading locations…")
                    .into_any_element(),
            );
        }

        let locations_section = div()
            .id("locations-section")
            .debug_selector(|| "locations-section".to_string())
            .child(sidebar_section_label("Locations", palette))
            .children(location_rows);

        let active_smart_folder = self.browser.active_smart_folder().map(|folder| folder.id());
        let smart_folders: Vec<_> = self
            .browser
            .smart_folders()
            .iter()
            .map(|folder| (folder.id(), folder.name().to_string()))
            .collect();
        let mut smart_rows = Vec::with_capacity(smart_folders.len().max(1));
        if smart_folders.is_empty() {
            smart_rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(palette.tertiary)
                    .child("Save a name search to create one")
                    .into_any_element(),
            );
        } else {
            for (index, (id, name)) in smart_folders.into_iter().enumerate() {
                smart_rows.push(
                    div()
                        .id(("smart-folder", index))
                        .group("smart-folder-row")
                        .role(Role::Button)
                        .aria_label(name.clone())
                        .aria_selected(active_smart_folder == Some(id))
                        .focusable()
                        .tab_stop(true)
                        .flex()
                        .items_center()
                        .gap_1()
                        .mx_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(if active_smart_folder == Some(id) {
                            palette.selected
                        } else {
                            palette.panel
                        })
                        .hover(move |row| row.bg(palette.hover))
                        .cursor_pointer()
                        .child(toolbar_icon("search", 14.0, palette.muted))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                        .child(
                            div()
                                .id(("edit-smart-folder", index))
                                .role(Role::Button)
                                .aria_label("Edit smart folder")
                                .focusable()
                                .tab_stop(true)
                                .invisible()
                                .px_1()
                                .rounded_sm()
                                .group_hover("smart-folder-row", |button| button.visible())
                                .focus(move |button| button.visible().bg(palette.hover))
                                .hover(move |button| button.visible().bg(palette.hover))
                                .cursor_pointer()
                                .child("✎")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_smart_folder_editor(Some(id), cx);
                                })),
                        )
                        .child(
                            div()
                                .id(("delete-smart-folder", index))
                                .role(Role::Button)
                                .aria_label("Delete smart folder")
                                .focusable()
                                .tab_stop(true)
                                .invisible()
                                .px_1()
                                .rounded(px(palette.radius))
                                .group_hover("smart-folder-row", |button| button.visible())
                                .focus(move |button| button.visible().bg(palette.hover))
                                .hover(move |button| button.visible().bg(palette.hover))
                                .cursor_pointer()
                                .child(toolbar_icon(
                                    "close",
                                    palette.icon_size.clamp(11.0, 15.0),
                                    palette.muted,
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.delete_smart_folder(id, cx);
                                })),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_smart_folder(id, cx);
                        }))
                        .into_any_element(),
                );
            }
        }

        let smart_folders_section = div()
            .id("smart-folders-section")
            .debug_selector(|| "smart-folders-section".to_string())
            .child(sidebar_section_label("Smart folders", palette))
            .children(smart_rows);
        let recents_section = div()
            .id("recents-section")
            .debug_selector(|| "recents-section".to_string())
            .child(sidebar_section_label("Recents", palette))
            .children(recent_rows);

        let scrollable_content = div()
            .id("sidebar-scroll")
            .debug_selector(|| "sidebar-scroll".to_string())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py_1()
            .overflow_y_scroll()
            .child(favorites_section)
            .child(smart_folders_section)
            .child(locations_section)
            .child(recents_section);
        let settings_button = div()
            .id("sidebar-settings")
            .debug_selector(|| "sidebar-settings".to_string())
            .role(Role::Button)
            .aria_label("Settings")
            .focusable()
            .tab_stop(true)
            .flex()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .mx_2()
            .px_2()
            .rounded_sm()
            .text_sm()
            .text_color(palette.muted)
            .focus(move |button| button.bg(palette.hover).text_color(palette.text))
            .hover(move |button| button.bg(palette.hover).text_color(palette.text))
            .cursor_pointer()
            .child(toolbar_icon("sliders", 15.0, palette.muted))
            .child("Settings")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_settings_panel(cx)));

        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".to_string())
            .role(Role::Navigation)
            .aria_label("Main navigation")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(self.layout.sidebar_width))
            .h_full()
            .overflow_hidden()
            .bg(self.palette.panel)
            .child(scrollable_content)
            .child(
                div()
                    .id("sidebar-footer")
                    .debug_selector(|| "sidebar-footer".to_string())
                    .flex_none()
                    .py_2()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .child(settings_button),
            )
            .into_any_element()
    }

    pub(crate) fn render_toolbar(&mut self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let density_scale = if palette.density == Density::Compact {
            0.94
        } else {
            1.0
        };
        let toolbar_button_size = 32.0 * palette.scale * density_scale;
        let popover_top = toolbar_button_size + 4.0;
        let breadcrumbs = self.render_breadcrumbs(compact, cx);
        let query_preview: String = self.browser.search_query().chars().take(20).collect();
        let search_label = if query_preview.is_empty() {
            "Search".to_string()
        } else {
            query_preview
        };
        let wide_plugin_badges = (!compact).then(|| self.render_plugin_badges(cx));
        let compact_plugin_badges = compact.then(|| self.render_plugin_badges(cx));
        let can_undo = self.undo_ledger.can_undo(SystemTime::now());
        let can_redo = self.undo_ledger.can_redo();
        let current_is_favorite = self.browser.is_favorite(self.browser.path());
        let favorite_label = if current_is_favorite {
            "Remove current from favorites"
        } else {
            "Add current to favorites"
        };
        let view_icon = match self.browser.view_mode() {
            ViewMode::List => "list",
            ViewMode::Grid => "grid",
            ViewMode::Column => "view-col",
        };
        let back_history = self
            .browser
            .back_history()
            .iter()
            .rev()
            .take(10)
            .cloned()
            .enumerate()
            .collect::<Vec<_>>();
        let forward_history = self
            .browser
            .forward_history()
            .iter()
            .rev()
            .take(10)
            .cloned()
            .enumerate()
            .collect::<Vec<_>>();
        let back_menu = back_history
            .into_iter()
            .map(|(index, path)| {
                history_menu_item(
                    ("back-history-item", index),
                    path_label(&path),
                    "back",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go_to_back_history(index, cx)))
                .into_any_element()
            })
            .collect();
        let forward_menu = forward_history
            .into_iter()
            .map(|(index, path)| {
                history_menu_item(
                    ("forward-history-item", index),
                    path_label(&path),
                    "forward",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go_to_forward_history(index, cx)))
                .into_any_element()
            })
            .collect();

        let create_menu = vec![
            toolbar_menu_item("new-folder", "New Folder", "folder", palette, false)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    this.prompt_new_folder(cx);
                }))
                .into_any_element(),
            toolbar_menu_item("new-note", "New Note", "file", palette, false)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    this.prompt_new_note(cx);
                }))
                .into_any_element(),
            toolbar_menu_item(
                "new-website-link",
                "New Website Link",
                "link",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.prompt_new_website_link(cx);
            }))
            .into_any_element(),
        ];
        let mut view_menu = vec![
            toolbar_menu_item(
                "list-view",
                "List",
                "list",
                palette,
                self.browser.view_mode() == ViewMode::List,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.set_view_mode(ViewMode::List, cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "column-view",
                "Column",
                "view-col",
                palette,
                self.browser.view_mode() == ViewMode::Column,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.set_view_mode(ViewMode::Column, cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "grid-view",
                "Grid",
                "grid",
                palette,
                self.browser.view_mode() == ViewMode::Grid,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.set_view_mode(ViewMode::Grid, cx);
            }))
            .into_any_element(),
            toolbar_menu_separator(palette),
            toolbar_menu_item(
                "hidden",
                if self.browser.show_hidden() {
                    "Hide Hidden Files"
                } else {
                    "Show Hidden Files"
                },
                if self.browser.show_hidden() {
                    "eye"
                } else {
                    "eye-closed"
                },
                palette,
                self.browser.show_hidden(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.toggle_hidden(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "view-preview",
                if self.settings.view.show_preview_panel {
                    "Hide Preview Panel"
                } else {
                    "Show Preview Panel"
                },
                "eye",
                palette,
                self.settings.view.show_preview_panel,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.toggle_preview_panel(cx);
            }))
            .into_any_element(),
        ];
        let view_menu_width = if self.browser.view_mode() == ViewMode::Grid {
            view_menu.push(toolbar_menu_separator(palette));
            view_menu.push(self.render_grid_width_control(cx));
            280.0
        } else {
            184.0
        };
        let sort_menu = [SortKey::Name, SortKey::Size, SortKey::Modified]
            .into_iter()
            .map(|key| {
                let active = self.browser.sort_key() == key;
                let label = if active {
                    format!(
                        "{} {}",
                        key.label(),
                        self.browser.sort_direction().indicator()
                    )
                } else {
                    key.label().to_string()
                };
                toolbar_menu_item(
                    match &key {
                        SortKey::Name => "toolbar-sort-name",
                        SortKey::Size => "toolbar-sort-size",
                        SortKey::Modified => "toolbar-sort-modified",
                        SortKey::Custom(_) => unreachable!("toolbar contains built-in sorts only"),
                    },
                    label,
                    "sort",
                    palette,
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    this.set_sort(key.clone(), cx);
                }))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let filter_menu = [EntryFilter::All, EntryFilter::Folders, EntryFilter::Files]
            .into_iter()
            .map(|filter| {
                toolbar_menu_item(
                    match filter {
                        EntryFilter::All => "toolbar-filter-all",
                        EntryFilter::Folders => "toolbar-filter-folders",
                        EntryFilter::Files => "toolbar-filter-files",
                    },
                    filter.label(),
                    match filter {
                        EntryFilter::All => "shield",
                        EntryFilter::Folders => "folder",
                        EntryFilter::Files => "file",
                    },
                    palette,
                    self.browser.filter() == filter,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    this.set_filter(filter, cx);
                }))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let more_menu = vec![
            toolbar_menu_item(
                "toolbar-theme",
                format!("Theme: {}", self.settings.appearance.theme.label()),
                if palette.dark { "sun" } else { "moon" },
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.cycle_theme(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "folder-sizes",
                if self.calculate_folder_sizes {
                    "Hide Folder Sizes"
                } else {
                    "Show Folder Sizes"
                },
                "folder",
                palette,
                self.calculate_folder_sizes,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.toggle_folder_sizes(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "go-to-folder-button",
                "Go to Folder…",
                "folder",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.open_go_to_folder(cx);
            }))
            .into_any_element(),
            toolbar_menu_separator(palette),
            toolbar_menu_item(
                "workspace-manager-button",
                "Workspaces",
                "grid",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.open_workspace_manager(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "remote-drive-manager-button",
                "Remote Drives",
                "link",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.open_remote_drive_manager(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "command-palette-button",
                "Commands",
                "search",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.open_control_surface(ControlSurface::CommandPalette, cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "shortcut-help-button",
                "Keyboard Shortcuts",
                "sliders",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.open_control_surface(ControlSurface::Shortcuts, cx);
            }))
            .into_any_element(),
            toolbar_menu_separator(palette),
            toolbar_menu_item(
                "refresh-preview-helpers",
                if self.preview.helpers_loading {
                    "Checking Preview Helpers…"
                } else {
                    "Check Preview Helpers"
                },
                "reload",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.start_preview_helpers(cx);
            }))
            .into_any_element(),
            toolbar_menu_item(
                "clear-preview-cache",
                "Clear Preview Cache",
                "trash",
                palette,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_toolbar_menu(cx);
                this.clear_preview_cache(cx);
            }))
            .into_any_element(),
        ];

        let back_button = compact_toolbar_button_with_tooltip(
            "back",
            "Go back",
            "arrow-left",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::BackHistory,
            self.browser.can_go_back(),
            shortcut_tooltip_text("Go back", &self.settings.shortcut_bindings, "nav-back"),
        )
        .on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.browser.can_go_back() {
                    this.toggle_toolbar_menu(ToolbarMenu::BackHistory, cx);
                }
            }),
        )
        .when(
            self.overlay.toolbar_menu == ToolbarMenu::BackHistory,
            |button| {
                button.relative().child(
                    deferred(div().absolute().top(px(popover_top)).left(px(0.0)).child(
                        toolbar_popover("back-history-menu", palette, back_menu).on_mouse_down_out(
                            cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                        ),
                    ))
                    .priority(1),
                )
            },
        );
        let forward_button = compact_toolbar_button_with_tooltip(
            "forward",
            "Go forward",
            "arrow-right",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::ForwardHistory,
            self.browser.can_go_forward(),
            shortcut_tooltip_text(
                "Go forward",
                &self.settings.shortcut_bindings,
                "nav-forward",
            ),
        )
        .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.browser.can_go_forward() {
                    this.toggle_toolbar_menu(ToolbarMenu::ForwardHistory, cx);
                }
            }),
        )
        .when(
            self.overlay.toolbar_menu == ToolbarMenu::ForwardHistory,
            |button| {
                button.relative().child(
                    deferred(
                        div().absolute().top(px(popover_top)).left(px(0.0)).child(
                            toolbar_popover("forward-history-menu", palette, forward_menu)
                                .on_mouse_down_out(
                                    cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                                ),
                        ),
                    )
                    .priority(1),
                )
            },
        );
        let create_button = compact_toolbar_button(
            "create-menu-button",
            "Create new item",
            "plus",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::Create,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::Create, cx)))
        .when(self.overlay.toolbar_menu == ToolbarMenu::Create, |button| {
            button.relative().child(
                deferred(div().absolute().top(px(popover_top)).left(px(0.0)).child(
                    toolbar_popover("create-menu", palette, create_menu).on_mouse_down_out(
                        cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                    ),
                ))
                .priority(1),
            )
        });
        let view_control = if compact {
            compact_toolbar_button(
                "view-menu-button",
                "Change view mode",
                view_icon,
                palette,
                self.overlay.toolbar_menu == ToolbarMenu::View,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::View, cx)))
            .when(self.overlay.toolbar_menu == ToolbarMenu::View, |button| {
                button.relative().child(
                    deferred(
                        div().absolute().top(px(popover_top)).right(px(0.0)).child(
                            toolbar_popover("view-menu", palette, view_menu)
                                .w(px(view_menu_width))
                                .on_mouse_down_out(
                                    cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                                ),
                        ),
                    )
                    .priority(1),
                )
            })
            .into_any_element()
        } else {
            let view_mode = self.browser.view_mode();
            let view_buttons = [
                ("view-list-button", "List view", "list", ViewMode::List),
                ("view-grid-button", "Grid view", "grid", ViewMode::Grid),
                (
                    "view-column-button",
                    "Column view",
                    "view-col",
                    ViewMode::Column,
                ),
            ]
            .into_iter()
            .map(|(id, label, icon, mode)| {
                compact_toolbar_button(id, label, icon, palette, view_mode == mode, true)
                    .role(Role::RadioButton)
                    .aria_selected(view_mode == mode)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_view_mode(mode, cx)))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
            let options_button = compact_toolbar_button(
                "view-options-button",
                "View options",
                "sliders",
                palette,
                self.overlay.toolbar_menu == ToolbarMenu::View,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::View, cx)))
            .when(self.overlay.toolbar_menu == ToolbarMenu::View, |button| {
                button.relative().child(
                    deferred(
                        div().absolute().top(px(popover_top)).right(px(0.0)).child(
                            toolbar_popover("view-menu", palette, view_menu)
                                .w(px(view_menu_width))
                                .on_mouse_down_out(
                                    cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                                ),
                        ),
                    )
                    .priority(1),
                )
            });
            div()
                .id("view-mode-control")
                .debug_selector(|| "view-mode-control".to_string())
                .role(Role::RadioGroup)
                .aria_label("View mode")
                .flex()
                .items_center()
                .gap_1()
                .children(view_buttons)
                .child(options_button)
                .into_any_element()
        };
        let sort_button = compact_toolbar_button(
            "sort",
            "Sort options",
            "sort",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::Sort,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::Sort, cx)))
        .when(self.overlay.toolbar_menu == ToolbarMenu::Sort, |button| {
            button.relative().child(
                deferred(div().absolute().top(px(popover_top)).right(px(0.0)).child(
                    toolbar_popover("sort-menu", palette, sort_menu).on_mouse_down_out(
                        cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                    ),
                ))
                .priority(1),
            )
        });
        let filter_button = compact_toolbar_button(
            "filter",
            "Filter options",
            "shield",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::Filter,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::Filter, cx)))
        .when(self.overlay.toolbar_menu == ToolbarMenu::Filter, |button| {
            button.relative().child(
                deferred(div().absolute().top(px(popover_top)).right(px(0.0)).child(
                    toolbar_popover("filter-menu", palette, filter_menu).on_mouse_down_out(
                        cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                    ),
                ))
                .priority(1),
            )
        });
        let more_button = compact_toolbar_button(
            "more-menu-button",
            "More options",
            "more-vertical",
            palette,
            self.overlay.toolbar_menu == ToolbarMenu::More,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_toolbar_menu(ToolbarMenu::More, cx)))
        .when(self.overlay.toolbar_menu == ToolbarMenu::More, |button| {
            button.relative().child(
                deferred(div().absolute().top(px(popover_top)).right(px(0.0)).child(
                    toolbar_popover("more-menu", palette, more_menu).on_mouse_down_out(
                        cx.listener(|this, _, _, cx| this.close_toolbar_menu(cx)),
                    ),
                ))
                .priority(1),
            )
        });
        let favorite_button = compact_toolbar_button(
            "toggle-favorite-button",
            favorite_label,
            "bookmark",
            palette,
            current_is_favorite,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_current_favorite(cx)));

        div()
            .id("browser-toolbar")
            .debug_selector(|| "browser-toolbar".to_string())
            .role(Role::Toolbar)
            .aria_label("File browser controls")
            .aria_orientation(Orientation::Horizontal)
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .min_w_0()
            .h(px(if compact { 72.0 } else { 40.0 }
                * palette.scale
                * density_scale))
            .when(compact, |toolbar| toolbar.flex_wrap())
            .child(back_button)
            .child(forward_button)
            .child(
                compact_toolbar_button_with_tooltip(
                    "up",
                    "Go to parent folder",
                    "arrow-up",
                    palette,
                    false,
                    self.browser.can_go_up(),
                    shortcut_tooltip_text(
                        "Go to parent folder",
                        &self.settings.shortcut_bindings,
                        "nav-up",
                    ),
                )
                .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
            )
            .child(
                compact_toolbar_button_with_tooltip(
                    "refresh",
                    "Refresh",
                    "reload",
                    palette,
                    false,
                    true,
                    shortcut_tooltip_text(
                        "Refresh",
                        &self.settings.shortcut_bindings,
                        "view-refresh",
                    ),
                )
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(breadcrumbs)
            .when_some(wide_plugin_badges, |toolbar, badge| toolbar.child(badge))
            .child(favorite_button)
            .child(create_button)
            .child(
                compact_toolbar_button_with_tooltip(
                    "undo",
                    "Undo",
                    "undo",
                    palette,
                    false,
                    can_undo,
                    shortcut_tooltip_text("Undo", &self.settings.shortcut_bindings, "edit-undo"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
            )
            .child(
                compact_toolbar_button_with_tooltip(
                    "redo",
                    "Redo",
                    "redo",
                    palette,
                    false,
                    can_redo,
                    shortcut_tooltip_text("Redo", &self.settings.shortcut_bindings, "edit-redo"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.redo(cx))),
            )
            .child(
                div()
                    .id("toolbar-trailing-controls")
                    .debug_selector(|| "toolbar-trailing-controls".to_string())
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!compact, |mut controls| {
                        // Give the search field's spare width to the path
                        // before the breadcrumbs start collapsing: these
                        // controls shrink far faster, down to the search
                        // field's minimum beside the view, sort, filter and
                        // more buttons (seven buttons, each after a gap).
                        controls.style().flex_shrink = Some(1_000.0);
                        controls
                            .min_w(px(112.0 + 7.0 * (toolbar_button_size + 4.0 * palette.scale)))
                    })
                    .when(compact, |controls| controls.w_full().justify_end())
                    .when_some(compact_plugin_badges, |controls, badge| {
                        controls.child(badge)
                    })
                    .child(
                        div()
                            .id("search")
                            .debug_selector(|| "search".to_string())
                            .role(Role::Button)
                            .aria_label("Search files and folders")
                            .focusable()
                            .tab_stop(true)
                            .flex()
                            .flex_shrink(1.)
                            .items_center()
                            .gap_1()
                            .min_w(px(112.0))
                            .w(px(180.0))
                            .when(compact, |search| search.flex_1())
                            .h(px(toolbar_button_size))
                            .px_2()
                            .border_1()
                            .border_color(if self.search.active {
                                palette.accent
                            } else {
                                palette.border
                            })
                            .focus(move |search| search.border_color(palette.accent))
                            .bg(if self.search.active {
                                palette.window
                            } else if palette.dark {
                                rgb(0x0b0b0b)
                            } else {
                                rgb(0xececec)
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.overlay.toolbar_menu = ToolbarMenu::Closed;
                                this.search.active = true;
                                let _ = window;
                                cx.notify();
                            }))
                            .child(if self.search.active {
                                self.native_text_input_element(TextInputTarget::Search)
                                    .unwrap_or_else(|| div().child(search_label).into_any_element())
                            } else {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(if self.browser.search_query().is_empty() {
                                        palette.tertiary
                                    } else {
                                        palette.text
                                    })
                                    .child(search_label)
                                    .into_any_element()
                            })
                            .when(!self.browser.search_query().is_empty(), |search| {
                                search
                                    .child(
                                        compact_toolbar_button(
                                            "clear-search",
                                            "Clear search",
                                            "close",
                                            palette,
                                            false,
                                            true,
                                        )
                                        .debug_selector(|| "clear-search".to_string())
                                        .w(px(24.0 * palette.scale))
                                        .h(px(24.0 * palette.scale))
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                // Don't let the field's own click handler
                                                // reactivate it.
                                                cx.stop_propagation();
                                                this.clear_search_and_focus_list(window, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        compact_toolbar_button(
                                            "save-search",
                                            "Save search as Smart Folder",
                                            "bookmark",
                                            palette,
                                            false,
                                            true,
                                        )
                                        .w(px(24.0 * palette.scale))
                                        .h(px(24.0 * palette.scale))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_current_search(cx)
                                            }),
                                        ),
                                    )
                            })
                            .when(self.browser.search_query().is_empty(), |search| {
                                search.child(toolbar_icon("search", 14.0, palette.tertiary))
                            }),
                    )
                    .child(view_control)
                    .child(sort_button)
                    .child(filter_button)
                    .child(more_button),
            )
            .into_any_element()
    }

    pub(crate) fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let name = sort_label(&self.browser, &SortKey::Name);
        let size = sort_label(&self.browser, &SortKey::Size);
        let modified = sort_label(&self.browser, &SortKey::Modified);
        let (columns, size_width, modified_width) = self.list_column_layout();
        let mut custom_headers = Vec::with_capacity(columns.len());
        for (column, width) in columns {
            let key = SortKey::Custom(column.clone());
            let label = sort_label(&self.browser, &key);
            let selector = format!("sort-custom-{}", column.to_lowercase());
            let aria_label = format!("Sort by custom field {column}");
            let width_key = format!("custom:{column}");
            let resize_handle = self.render_list_column_resize_handle(
                width_key,
                width,
                format!("Resize {column} column"),
                cx,
            );
            custom_headers.push(
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector.clone())
                    .role(Role::Button)
                    .aria_label(aria_label)
                    .focusable()
                    .tab_stop(true)
                    .w(px(width))
                    .min_w(px(width))
                    .relative()
                    .px_2()
                    .truncate()
                    .focus(move |header| header.bg(palette.hover).text_color(palette.text))
                    .hover(move |header| header.bg(palette.hover).text_color(palette.text))
                    .cursor_pointer()
                    .child(label)
                    .child(resize_handle)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_sort(key.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }

        let size_resize = self.render_list_column_resize_handle(
            "size".to_string(),
            size_width,
            "Resize size column".to_string(),
            cx,
        );
        let modified_resize = self.render_list_column_resize_handle(
            "modified".to_string(),
            modified_width,
            "Resize modified column".to_string(),
            cx,
        );

        div()
            .id("listing-header")
            .flex()
            .w_full()
            .h(px(30.0 * self.palette.scale))
            .items_center()
            .border_b_1()
            .border_color(self.palette.border)
            .bg(self.palette.surface)
            .text_xs()
            .text_color(self.palette.muted)
            .child(
                div()
                    .id("sort-name")
                    .debug_selector(|| "sort-name".to_string())
                    .role(Role::Button)
                    .aria_label("Sort by name")
                    .focusable()
                    .tab_stop(true)
                    .flex_1()
                    .px_3()
                    .focus(move |header| header.bg(palette.hover).text_color(palette.text))
                    .hover(move |header| header.bg(palette.hover).text_color(palette.text))
                    .cursor_pointer()
                    .child(name)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_sort(SortKey::Name, cx);
                    })),
            )
            .children(custom_headers)
            .child(
                div()
                    .id("sort-size")
                    .debug_selector(|| "sort-size".to_string())
                    .role(Role::Button)
                    .aria_label("Sort by size")
                    .focusable()
                    .tab_stop(true)
                    .w(px(size_width))
                    .relative()
                    .flex_none()
                    .px_2()
                    .focus(move |header| header.bg(palette.hover).text_color(palette.text))
                    .hover(move |header| header.bg(palette.hover).text_color(palette.text))
                    .cursor_pointer()
                    .child(size)
                    .child(size_resize)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_sort(SortKey::Size, cx);
                    })),
            )
            .child(
                div()
                    .id("sort-modified")
                    .debug_selector(|| "sort-modified".to_string())
                    .role(Role::Button)
                    .aria_label("Sort by modified date")
                    .focusable()
                    .tab_stop(true)
                    .w(px(modified_width))
                    .relative()
                    .flex_none()
                    .px_2()
                    .focus(move |header| header.bg(palette.hover).text_color(palette.text))
                    .hover(move |header| header.bg(palette.hover).text_color(palette.text))
                    .cursor_pointer()
                    .child(modified)
                    .child(modified_resize)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_sort(SortKey::Modified, cx);
                    })),
            )
            .into_any_element()
    }
}
