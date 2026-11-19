use super::*;
use std::path::Path;

pub(super) const FILTER_ID: &str = "files-filter";

pub(super) struct FileClipboard {
    names: Vec<String>,
    paths: String,
    cut: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_navigation_in_files_centers_the_active_row_and_clamps_at_the_edges() {
        let dir = tempfile::tempdir().unwrap();
        let image = image::RgbImage::new(8, 6);
        for index in 0..50 {
            image
                .save(dir.path().join(format!("{index:02}.png")))
                .unwrap();
        }
        let ctx = egui::Context::default();
        let workspace = Workspace::open(dir.path()).unwrap();
        let mut app = App {
            language: Language::English,
            images: workspace.images().unwrap().to_vec(),
            workspace: Some(workspace),
            ..Default::default()
        };
        app.filter();
        app.load(&ctx, 24);
        let render = |app: &mut App, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(450., 340.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.file_shortcuts(ctx);
                    app.navigation_shortcut(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| app.files_ui(ui));
                },
            )
        };
        let row = |output: &egui::FullOutput, name: &str| {
            output
                .shapes
                .iter()
                .find_map(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape
                        && text.galley.text() == name
                    {
                        return Some((text.pos + text.galley.size() / 2., clipped.clip_rect));
                    }
                    None
                })
                .unwrap()
        };
        let output = render(&mut app, vec![]);
        let (center, viewport) = row(&output, "24.png");
        assert!((center.y - viewport.center().y).abs() < 5.);
        for pressed in [true, false] {
            let _ = render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(center),
                    egui::Event::PointerButton {
                        pos: center,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        for (key, expected) in [
            (Key::ArrowDown, "25.png"),
            (Key::D, "26.png"),
            (Key::ArrowUp, "25.png"),
            (Key::A, "24.png"),
            (Key::ArrowRight, "25.png"),
            (Key::ArrowLeft, "24.png"),
        ] {
            let output = render(
                &mut app,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(app.frame.as_ref().unwrap().name, expected);
            assert_eq!(app.selected_file_names(), [expected]);
            let (center, viewport) = row(&output, expected);
            assert!((center.y - viewport.center().y).abs() < 5.);
        }
        app.load(&ctx, 0);
        let (top, viewport) = row(&render(&mut app, vec![]), "00.png");
        assert!(top.y - viewport.top() < 20.);
        app.load(&ctx, 49);
        let (end, viewport) = row(&render(&mut app, vec![]), "49.png");
        assert!(viewport.bottom() - end.y < 20.);
    }

    #[test]
    fn file_panel_selects_rows_and_undoes_deletion_as_a_group() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbImage::new(8, 6)
                .save(dir.path().join(name))
                .unwrap();
        }
        let annotation = r#"{"imagePath":"a.png","shapes":[{"label":"car","shape_type":"point","points":[[1,2]]}]}"#;
        std::fs::write(dir.path().join("a.json"), annotation).unwrap();
        let ctx = egui::Context::default();
        let workspace = Workspace::open(dir.path()).unwrap();
        let mut app = App {
            language: Language::English,
            images: workspace.images().unwrap().to_vec(),
            workspace: Some(workspace),
            ..Default::default()
        };
        app.filter();
        let render = |app: &mut App, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(450., 340.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.file_shortcuts(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| app.files_ui(ui));
                },
            )
        };
        let text_position = |output: &egui::FullOutput, text: &str| {
            output.shapes.iter().find_map(|clipped| {
                if let egui::Shape::Text(shape) = &clipped.shape
                    && shape.galley.text() == text
                {
                    return Some(shape.pos + vec2(3., 6.));
                }
                None
            })
        };
        let click = |app: &mut App, pos, button, modifiers| {
            for pressed in [true, false] {
                let _ = render(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            modifiers,
                        },
                    ],
                );
            }
        };
        let output = render(&mut app, vec![]);
        let a = text_position(&output, "a.png").unwrap();
        let b = text_position(&output, "b.png").unwrap();
        let c = text_position(&output, "c.png").unwrap();
        click(
            &mut app,
            a,
            egui::PointerButton::Primary,
            egui::Modifiers::NONE,
        );
        assert_eq!(app.selected_file_names(), ["a.png"]);
        click(
            &mut app,
            b,
            egui::PointerButton::Secondary,
            egui::Modifiers::NONE,
        );
        assert_eq!(app.selected_file_names(), ["b.png"]);
        assert_eq!(app.frame.as_ref().unwrap().name, "a.png");
        render(&mut app, vec![]);
        let menu = render(&mut app, vec![]);
        for text in ["Cut", "Copy", "Paste", "Rename…", "Delete…"] {
            assert!(
                text_position(&menu, text).is_some(),
                "{text} is missing from the file menu"
            );
        }

        let _ = render(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        app.select_file(0, egui::Modifiers::CTRL);
        click(
            &mut app,
            b,
            egui::PointerButton::Secondary,
            egui::Modifiers::NONE,
        );
        assert_eq!(app.selected_file_names(), ["a.png", "b.png"]);

        let _ = render(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let primary = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for events in [
            vec![egui::Event::PointerMoved(a)],
            vec![primary(a, true)],
            vec![egui::Event::PointerMoved(c)],
            vec![primary(c, false)],
        ] {
            let _ = render(&mut app, events);
        }
        let selected = app.selected_file_names();
        assert_eq!(selected, ["a.png", "b.png", "c.png"]);
        assert_eq!(app.frame.as_ref().unwrap().name, "a.png");

        app.delete_files(&ctx, &selected, false);
        assert!(app.images.is_empty());
        assert!(!dir.path().join("a.json").exists());
        app.focus_file_panel(&ctx);
        let _ = render(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
        );
        assert_eq!(app.images, ["a.png", "b.png", "c.png"]);
        assert_eq!(app.frame.as_ref().unwrap().name, "a.png");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.json")).unwrap(),
            annotation
        );
        assert!(app.file_undo.is_empty());
    }
}

pub(super) enum FileDialog {
    Rename {
        image: String,
        filename: String,
        focus: bool,
        error: String,
    },
    RenameClass {
        from: String,
        to: String,
    },
    Delete {
        annotations_only: bool,
    },
}

enum FileAction {
    Copy { cut: bool },
    Paste(PathBuf),
    CopyPaths,
    Reveal(PathBuf),
    Rename,
    RenameClass,
    Delete { annotations_only: bool },
    SelectAll,
    Undo,
}

pub(super) struct FileUndo {
    changes: Vec<FileChange>,
    active_image: Option<String>,
}

enum FileChange {
    Archived {
        name: String,
        path: PathBuf,
        annotations_only: bool,
    },
    Copied(String),
    Moved {
        name: String,
        original: String,
    },
    RenamedClass {
        name: String,
        revision: String,
        indices: Vec<usize>,
        previous: String,
    },
}

impl App {
    pub(super) fn files_ui(&mut self, ui: &mut egui::Ui) {
        let lang = self.language;
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.filter)
                    .id(egui::Id::new(FILTER_ID))
                    .hint_text(lang.text("Filter by filename", "Lọc theo tên tệp"))
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            self.filter();
        }
        let mut action = None;
        ui.horizontal(|ui| {
            let count = ui.add(
                egui::Label::new(format!("{} / {}", self.filtered.len(), self.images.len()))
                    .sense(egui::Sense::click()),
            );
            self.file_panel_focus = Some(count.id);
            if count.clicked() {
                self.focus_file_panel(ui.ctx());
            }
            count.context_menu(|ui| action = self.paste_menu(ui, None));
            if self.selected_files.len() > 1 {
                ui.weak(lang.text(
                    format!("{} selected", self.selected_files.len()),
                    format!("Đã chọn {}", self.selected_files.len()),
                ));
            }
            if ui
                .add_enabled(
                    self.can_manage_files(),
                    egui::Button::new(lang.text("Refresh", "Cập nhật")).small(),
                )
                .on_hover_text(lang.text(
                    "Refresh images and classes",
                    "Cập nhật danh sách ảnh và lớp",
                ))
                .clicked()
            {
                self.refresh_workspace(ui.ctx());
                self.focus_file_panel(ui.ctx());
            }
        });
        ui.separator();
        let mut open = None;
        let row_height = ui.spacing().interact_size.y + 2.;
        let modifiers = ui.input(|i| i.modifiers);
        let (pointer, dragging) =
            ui.input(|input| (input.pointer.interact_pos(), input.pointer.primary_down()));
        let mut drag_target = None;
        let height = ui.available_height().max(0.);
        let row = self
            .frame
            .as_ref()
            .and_then(|frame| self.images.binary_search(&frame.name).ok())
            .and_then(|index| self.filtered.binary_search(&index).ok());
        let center = row.map(|row| (row, self.filtered.len(), height));
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("files-list")
            .auto_shrink([false, true]);
        if self.centered_file_row != center {
            self.centered_file_row = center;
            if let Some((row, total, height)) = center {
                let spacing = ui.spacing().item_spacing.y;
                let stride = row_height + spacing;
                let max_offset = (total as f32 * stride - spacing - height).max(0.);
                let offset = row as f32 * stride + (row_height - height) / 2.;
                scroll = scroll.vertical_scroll_offset(offset.clamp(0., max_offset));
            }
        }
        scroll.show_rows(ui, row_height, self.filtered.len(), |ui, rows| {
            ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
            for row in rows {
                let index = self.filtered[row];
                let name = self.images[index].clone();
                let current = self.frame.as_ref().is_some_and(|f| f.name == name);
                let marked = self.selected_files.contains(&index);
                let has_json = self
                    .workspace
                    .as_ref()
                    .and_then(|ws| ws.stamp(&name).ok().flatten())
                    .is_some();
                let cut = self
                    .file_clipboard
                    .as_ref()
                    .is_some_and(|clipboard| clipboard.cut && clipboard.names.contains(&name));
                let text = if cut {
                    egui::RichText::new(&name).weak()
                } else {
                    egui::RichText::new(&name)
                };
                let mut button = egui::Button::new(text)
                    .truncate()
                    .sense(egui::Sense::click_and_drag())
                    .corner_radius(3)
                    .min_size(vec2(ui.available_width(), row_height))
                    .selected(marked);
                if current && !marked {
                    button = button.stroke(ui.visuals().selection.stroke);
                }
                if has_json {
                    button = button.shortcut_text(
                        egui::RichText::new("✓").color(Color32::from_rgb(100, 200, 120)),
                    );
                }
                let response = ui
                    .push_id(&name, |ui| ui.add(button))
                    .inner
                    .on_hover_text(&name);
                if let Some(pointer) = pointer
                    && response
                        .interact_rect
                        .expand2(vec2(0., ui.spacing().item_spacing.y / 2.))
                        .contains(pointer)
                {
                    drag_target = Some(index);
                }
                if response.drag_started_by(egui::PointerButton::Primary) {
                    self.file_drag_anchor = Some(index);
                    self.last_file_click = Some(index);
                    self.focus_file_panel(ui.ctx());
                    self.select_file_range(index, index, false);
                }
                if response.clicked() {
                    response.request_focus();
                    self.file_focus = Some(response.id);
                    if self.select_file(index, modifiers) {
                        open = Some(index);
                    }
                }
                if response.secondary_clicked() {
                    response.request_focus();
                    self.file_focus = Some(response.id);
                    if !marked {
                        self.select_file(index, egui::Modifiers::NONE);
                    }
                }
                response.context_menu(|ui| {
                    if let Some(chosen) = self.file_menu(ui, &name) {
                        action = Some(chosen);
                    }
                });
            }
        });
        if let Some(anchor) = self.file_drag_anchor {
            if let Some(target) = drag_target {
                self.select_file_range(anchor, target, false);
            }
            if !dragging {
                self.file_drag_anchor = None;
            }
        }
        if self.workspace.is_some() && ui.available_height() > 0. {
            let blank = ui.allocate_response(ui.available_size(), egui::Sense::click());
            if blank.clicked() || blank.secondary_clicked() {
                self.selected_files.clear();
                self.last_file_click = None;
                blank.request_focus();
                self.file_focus = Some(blank.id);
            }
            blank.context_menu(|ui| {
                if let Some(chosen) = self.paste_menu(ui, None) {
                    action = Some(chosen);
                }
            });
        }
        if let Some(index) = open {
            if self
                .frame
                .as_ref()
                .is_none_or(|f| f.name != self.images[index])
            {
                self.load(ui.ctx(), index);
            }
        }
        if let Some(action) = action {
            self.file_action(ui.ctx(), action);
        }
        if let Some(id) = self.file_focus {
            ui.ctx().memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
        }
    }

    fn select_file(&mut self, index: usize, modifiers: egui::Modifiers) -> bool {
        let toggle = modifiers.command || modifiers.ctrl;
        if modifiers.shift
            && let Some(anchor) = self.last_file_click
        {
            self.select_file_range(anchor, index, toggle);
            return false;
        }
        self.last_file_click = Some(index);
        if toggle {
            if !self.selected_files.remove(&index) {
                self.selected_files.insert(index);
            }
            return false;
        }
        self.selected_files.clear();
        self.selected_files.insert(index);
        !modifiers.shift
    }

    fn select_file_range(&mut self, anchor: usize, index: usize, extend: bool) {
        if !extend {
            self.selected_files.clear();
        }
        let range = anchor.min(index)..=anchor.max(index);
        self.selected_files
            .extend(self.filtered.iter().copied().filter(|i| range.contains(i)));
    }

    fn file_menu(&self, ui: &mut egui::Ui, name: &str) -> Option<FileAction> {
        let lang = self.language;
        let path = self.workspace.as_ref()?.image_path(name).ok()?;
        let mut action = None;
        let single = self.selected_files.len() == 1;
        let ready = self.can_manage_files();
        for (label, shortcut, command) in [
            (
                lang.text("Cut", "Cắt"),
                "Ctrl+X",
                FileAction::Copy { cut: true },
            ),
            (
                lang.text("Copy", "Sao chép"),
                "Ctrl+C",
                FileAction::Copy { cut: false },
            ),
        ] {
            if ui
                .add(egui::Button::new(label).shortcut_text(shortcut))
                .clicked()
            {
                action = Some(command);
            }
        }
        if let Some(paste) = self.paste_menu(ui, path.parent()) {
            action = Some(paste);
        }
        ui.separator();
        if ui
            .button(lang.text("Copy Path", "Chép đường dẫn"))
            .clicked()
        {
            action = Some(FileAction::CopyPaths);
        }
        if ui
            .button(lang.text("Reveal in File Manager", "Hiện trong trình quản lý tệp"))
            .clicked()
        {
            action = Some(FileAction::Reveal(clean_path(&path)));
        }
        ui.separator();
        if ui
            .add_enabled(
                ready && single,
                egui::Button::new(lang.text("Rename…", "Đổi tên…")).shortcut_text("F2"),
            )
            .clicked()
        {
            action = Some(FileAction::Rename);
        }
        if ui
            .add_enabled(
                ready,
                egui::Button::new(lang.text("Delete…", "Xóa…")).shortcut_text("Del"),
            )
            .clicked()
        {
            action = Some(FileAction::Delete {
                annotations_only: false,
            });
        }
        ui.add_enabled_ui(ready, |ui| {
            ui.menu_button(lang.text("Annotations", "Nhãn"), |ui| {
                if ui
                    .button(lang.text("Rename Class…", "Đổi tên lớp…"))
                    .clicked()
                {
                    action = Some(FileAction::RenameClass);
                }
                if ui
                    .button(lang.text("Delete Annotations…", "Xóa tệp nhãn…"))
                    .clicked()
                {
                    action = Some(FileAction::Delete {
                        annotations_only: true,
                    });
                }
            });
        });
        if action.is_some() {
            ui.close_menu();
        }
        action
    }

    fn paste_menu(&self, ui: &mut egui::Ui, folder: Option<&Path>) -> Option<FileAction> {
        let enabled = self.can_manage_files() && self.file_clipboard.is_some();
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(self.language.text("Paste", "Dán")).shortcut_text("Ctrl+V"),
            )
            .clicked()
        {
            ui.close_menu();
            let folder = folder.or_else(|| self.workspace.as_ref().map(Workspace::root))?;
            return Some(FileAction::Paste(folder.to_owned()));
        }
        None
    }

    fn can_manage_files(&self) -> bool {
        self.workspace.is_some()
            && self.scan.is_none()
            && self.format_job.is_none()
            && !self.pending()
            && self.annotation_drag.is_none()
            && self.start.is_none()
    }

    pub(super) fn files_have_focus(&self, ctx: &egui::Context) -> bool {
        self.file_focus
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
    }

    pub(super) fn focus_file_panel(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.file_panel_focus {
            ctx.memory_mut(|memory| {
                if !memory.has_focus(id) {
                    memory.request_focus(id);
                }
            });
            self.file_focus = Some(id);
        }
    }

    pub(super) fn file_shortcuts(&mut self, ctx: &egui::Context) {
        // egui may move focus at the end of a pass for an arrow key it saw before we consumed it.
        if std::mem::take(&mut self.restore_files_focus)
            && self.file_dialog.is_none()
            && self.label_draft.is_none()
            && !self.discard
            && !ctx.input(|input| {
                input.pointer.any_pressed()
                    || input.pointer.any_released()
                    || input.key_pressed(Key::Tab)
                    || input.key_pressed(Key::Escape)
            })
        {
            self.focus_file_panel(ctx);
        }
        if self.file_dialog.is_some()
            || self.label_draft.is_some()
            || self.discard
            || !self.files_have_focus(ctx)
        {
            return;
        }
        let action = ctx.input_mut(|input| {
            if !input.modifiers.shift
                && !input.modifiers.alt
                && (input.consume_key(egui::Modifiers::COMMAND, Key::Z)
                    || input.consume_key(egui::Modifiers::CTRL, Key::Z))
            {
                return Some(FileAction::Undo);
            }
            if input.consume_key(egui::Modifiers::COMMAND, Key::A) {
                return Some(FileAction::SelectAll);
            }
            if input.consume_key(egui::Modifiers::NONE, Key::F2) {
                return Some(FileAction::Rename);
            }
            if input.consume_key(egui::Modifiers::NONE, Key::Delete) {
                return Some(FileAction::Delete {
                    annotations_only: false,
                });
            }
            input.events.iter().find_map(|event| match event {
                egui::Event::Copy => Some(FileAction::Copy { cut: false }),
                egui::Event::Cut => Some(FileAction::Copy { cut: true }),
                egui::Event::Paste(text)
                    if self
                        .file_clipboard
                        .as_ref()
                        .is_some_and(|clipboard| clipboard.paths == *text) =>
                {
                    let workspace = self.workspace.as_ref()?;
                    let folder = self
                        .last_file_click
                        .and_then(|index| self.images.get(index))
                        .and_then(|name| workspace.image_path(name).ok())
                        .and_then(|path| path.parent().map(Path::to_owned))
                        .unwrap_or_else(|| workspace.root().to_owned());
                    Some(FileAction::Paste(folder))
                }
                _ => None,
            })
        });
        if let Some(action) = action {
            self.file_action(ctx, action);
        }
    }

    pub(super) fn selected_file_names(&self) -> Vec<String> {
        self.selected_files
            .iter()
            .filter_map(|&index| self.images.get(index))
            .cloned()
            .collect()
    }

    fn file_action(&mut self, ctx: &egui::Context, action: FileAction) {
        if matches!(
            action,
            FileAction::Paste(_)
                | FileAction::Rename
                | FileAction::RenameClass
                | FileAction::Delete { .. }
                | FileAction::Undo
        ) && !self.can_manage_files()
        {
            self.status = self
                .language
                .text(
                    "Finish or save the current edit before changing files.",
                    "Hãy hoàn tất hoặc lưu chỉnh sửa trước khi thao tác với tệp.",
                )
                .into();
            return;
        }
        match action {
            FileAction::Copy { .. } | FileAction::CopyPaths => {
                let Some(workspace) = &self.workspace else {
                    return;
                };
                let names = self.selected_file_names();
                if names.is_empty() {
                    return;
                }
                let paths = names
                    .iter()
                    .filter_map(|name| workspace.image_path(name).ok())
                    .map(|path| clean_path(&path).display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                if paths.is_empty() {
                    return;
                }
                ctx.copy_text(paths.clone());
                if let FileAction::Copy { cut } = action {
                    let verb = if cut {
                        self.language.text("Cut", "Đã cắt")
                    } else {
                        self.language.text("Copied", "Đã sao chép")
                    };
                    let count = names.len();
                    self.status = self.language.text(
                        format!("{verb} {count} image(s)."),
                        format!("{verb} {count} ảnh."),
                    );
                    self.file_clipboard = Some(FileClipboard { names, paths, cut });
                } else {
                    self.status = self.language.text(
                        format!("Copied {} file path(s).", names.len()),
                        format!("Đã chép {} đường dẫn tệp.", names.len()),
                    );
                }
            }
            FileAction::Paste(folder) => self.paste_files(ctx, &folder),
            FileAction::Undo => self.undo_file_action(ctx),
            FileAction::Reveal(path) => reveal_in_file_manager(&path),
            FileAction::Rename => {
                let names = self.selected_file_names();
                if let [image] = names.as_slice() {
                    let filename = image.rsplit('/').next().unwrap_or(image).to_owned();
                    self.file_dialog = Some(FileDialog::Rename {
                        image: image.clone(),
                        filename,
                        focus: true,
                        error: String::new(),
                    });
                }
            }
            FileAction::RenameClass if !self.selected_files.is_empty() => {
                self.file_dialog = Some(FileDialog::RenameClass {
                    from: String::new(),
                    to: String::new(),
                });
            }
            FileAction::Delete { annotations_only } if !self.selected_files.is_empty() => {
                self.file_dialog = Some(FileDialog::Delete { annotations_only });
            }
            FileAction::SelectAll => self.selected_files = self.filtered.iter().copied().collect(),
            _ => {}
        }
    }

    pub(super) fn show_file_dialog(&mut self, ctx: &egui::Context) {
        let lang = self.language;
        let Some(mut dialog) = self.file_dialog.take() else {
            return;
        };
        let names = self.selected_file_names();
        let modal = egui::Modal::new(egui::Id::new("files-dialog")).show(ctx, |ui| {
            ui.set_min_width(300.);
            let accepted = match &mut dialog {
                FileDialog::Rename {
                    filename,
                    focus,
                    error,
                    ..
                } => {
                    ui.heading(lang.text("Rename Image", "Đổi tên ảnh"));
                    let edit = ui.add(egui::TextEdit::singleline(filename).desired_width(300.));
                    if std::mem::take(focus) {
                        edit.request_focus();
                    }
                    if !error.is_empty() {
                        ui.colored_label(Color32::LIGHT_RED, error.as_str());
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    (
                        !filename.trim().is_empty(),
                        lang.text("Rename", "Đổi tên"),
                        enter,
                    )
                }
                FileDialog::RenameClass { from, to } => {
                    ui.heading(lang.text("Rename Class", "Đổi tên lớp"));
                    ui.label(lang.text(
                        format!("{} selected image(s)", names.len()),
                        format!("Đã chọn {} ảnh", names.len()),
                    ));
                    ui.add(
                        egui::TextEdit::singleline(from)
                            .hint_text(lang.text("Current class", "Lớp hiện tại"))
                            .desired_width(300.),
                    );
                    ui.add(
                        egui::TextEdit::singleline(to)
                            .hint_text(lang.text("New class", "Lớp mới"))
                            .desired_width(300.),
                    );
                    (
                        !from.trim().is_empty()
                            && !to.trim().is_empty()
                            && from.trim() != to.trim(),
                        lang.text("Rename", "Đổi tên"),
                        false,
                    )
                }
                FileDialog::Delete { annotations_only } => {
                    ui.heading(if *annotations_only {
                        lang.text("Delete Annotations?", "Xóa tệp nhãn?")
                    } else {
                        lang.text("Delete Images?", "Xóa ảnh?")
                    });
                    if let [name] = names.as_slice() {
                        ui.label(name);
                    } else {
                        ui.label(lang.text(
                            format!("{} selected images", names.len()),
                            format!("Đã chọn {} ảnh", names.len()),
                        ));
                    }
                    ui.label(if *annotations_only {
                        lang.text(
                            "Annotation JSON files will move to .rustlabel/trash.",
                            "Tệp nhãn JSON sẽ được chuyển vào .rustlabel/trash.",
                        )
                    } else {
                        lang.text(
                            "Images and their Labelme JSON files will move to .rustlabel/trash.",
                            "Ảnh và tệp nhãn Labelme JSON sẽ được chuyển vào .rustlabel/trash.",
                        )
                    });
                    (true, lang.text("Delete", "Xóa"), false)
                }
            };
            let (enabled, label, enter) = accepted;
            ui.horizontal(|ui| {
                let cancel = ui.button(lang.text("Cancel", "Hủy")).clicked();
                let accept = ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                    || (enabled && enter);
                (cancel, accept)
            })
            .inner
        });
        let (cancel, accept) = modal.inner;
        if cancel || modal.should_close() {
            self.focus_file_panel(ctx);
            return;
        }
        if accept && self.can_manage_files() {
            match &mut dialog {
                FileDialog::Rename {
                    image,
                    filename,
                    error,
                    ..
                } => match self.rename_image(ctx, image, filename.trim()) {
                    Ok(()) => {
                        self.focus_file_panel(ctx);
                        return;
                    }
                    Err(err) => *error = format!("{err:#}"),
                },
                FileDialog::RenameClass { from, to } => {
                    self.apply_batch_relabel(&names, from.trim(), to.trim());
                    self.focus_file_panel(ctx);
                    return;
                }
                FileDialog::Delete { annotations_only } => {
                    self.delete_files(ctx, &names, *annotations_only);
                    self.focus_file_panel(ctx);
                    return;
                }
            }
        }
        self.file_dialog = Some(dialog);
    }

    fn rename_image(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        filename: &str,
    ) -> anyhow::Result<()> {
        let workspace = self
            .workspace
            .as_mut()
            .expect("file actions require a workspace");
        let source = workspace.image_path(name)?;
        let target =
            workspace.transfer_image(name, source.parent().unwrap(), Some(filename), true)?;
        if name != target {
            self.record_file_action(
                vec![FileChange::Moved {
                    name: target.clone(),
                    original: name.to_owned(),
                }],
                None,
            );
        }
        self.sync_file_inventory(
            ctx,
            std::slice::from_ref(&target),
            &[(name.to_owned(), target.clone())],
        );
        self.status = self.language.text(
            format!("Renamed to {target}."),
            format!("Đã đổi tên thành {target}."),
        );
        Ok(())
    }

    fn paste_files(&mut self, ctx: &egui::Context, folder: &Path) {
        let Some(mut clipboard) = self.file_clipboard.take() else {
            return;
        };
        let workspace = self
            .workspace
            .as_mut()
            .expect("file actions require a workspace");
        let mut pasted = Vec::new();
        let mut moved = Vec::new();
        let mut changes = Vec::new();
        let mut failed = Vec::new();
        let mut last_error = None;
        for name in &clipboard.names {
            match workspace.transfer_image(name, folder, None, clipboard.cut) {
                Ok(target) => {
                    if clipboard.cut && target != *name {
                        moved.push((name.clone(), target.clone()));
                        changes.push(FileChange::Moved {
                            name: target.clone(),
                            original: name.clone(),
                        });
                    } else if !clipboard.cut {
                        changes.push(FileChange::Copied(target.clone()));
                    }
                    pasted.push(target);
                }
                Err(error) => {
                    failed.push(name.clone());
                    last_error = Some(format!("{name}: {error:#}"));
                }
            }
        }
        if !pasted.is_empty() {
            self.sync_file_inventory(ctx, &pasted, &moved);
            self.record_file_action(changes, None);
            self.focus_file_panel(ctx);
        }
        if clipboard.cut {
            clipboard.names = failed.clone();
        }
        if !clipboard.names.is_empty() {
            self.file_clipboard = Some(clipboard);
        }
        self.status = self.language.text(
            format!("Pasted {} image(s).", pasted.len()),
            format!("Đã dán {} ảnh.", pasted.len()),
        );
        if let Some(error) = last_error {
            self.status.push_str(&self.language.text(
                format!(" {} failed. {error}", failed.len()),
                format!(" Không dán được {} ảnh. {error}", failed.len()),
            ));
        }
    }

    pub(super) fn sync_file_inventory(
        &mut self,
        ctx: &egui::Context,
        selected: &[String],
        moved: &[(String, String)],
    ) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let Ok(images) = workspace.images() else {
            return;
        };
        let current = self.frame.as_ref().map(|f| f.name.clone());
        let previous_index = current
            .as_ref()
            .and_then(|name| self.images.binary_search(name).ok());
        if self.images != images {
            self.workspace_cache_dirty = true;
            if self.workspace_cache_next_image.is_some() {
                self.workspace_cache_next_image = Some(0);
            }
        }
        self.images = images.to_vec();
        self.filter();
        self.selected_files = selected
            .iter()
            .filter_map(|name| self.images.binary_search(name).ok())
            .collect();
        self.last_file_click = self.selected_files.first().copied();
        if let Some(name) = current {
            if let Some((_, new_name)) = moved.iter().find(|(from, _)| *from == name) {
                let frame = self.frame.as_mut().unwrap();
                frame.name.clone_from(new_name);
                match workspace.inspect(new_name) {
                    Ok(snapshot) => frame.snapshot = snapshot,
                    Err(_) => {
                        self.frame = None;
                        self.tag_rects.clear();
                        if let Ok(index) = self.images.binary_search(new_name) {
                            self.load(ctx, index);
                        }
                    }
                }
            } else if self.images.binary_search(&name).is_err() {
                self.frame = None;
                self.tag_rects.clear();
                let index = previous_index.unwrap_or_default();
                if let Some(&next) = self
                    .filtered
                    .iter()
                    .find(|&&i| i >= index)
                    .or_else(|| self.filtered.last())
                {
                    self.load(ctx, next);
                } else if let Some(workspace) = &self.workspace {
                    self.workspace_last_images
                        .remove(&clean_path(workspace.root()));
                }
            }
        }
        self.save_session_cache();
    }

    pub(super) fn apply_batch_relabel(&mut self, names: &[String], from: &str, to: &str) {
        let Some(ws) = &self.workspace else { return };
        if let Err(error) = validate_label(to) {
            self.status = error.to_string();
            return;
        }
        let mut renamed = 0;
        let mut changes = Vec::new();
        let mut failed = 0;
        for name in names {
            let result = (|| -> anyhow::Result<Option<FileChange>> {
                let mut snapshot = ws.inspect(name)?;
                let mut indices = Vec::new();
                for (index, item) in shapes_mut(&mut snapshot.doc)?.iter_mut().enumerate() {
                    if item["label"] == from {
                        item["label"] = serde_json::json!(to);
                        indices.push(index);
                    }
                }
                if indices.is_empty() {
                    return Ok(None);
                }
                let revision = ws.save(name, &snapshot.revision, &snapshot.doc)?;
                renamed += indices.len();
                Ok(Some(FileChange::RenamedClass {
                    name: name.clone(),
                    revision,
                    indices,
                    previous: from.to_owned(),
                }))
            })();
            match result {
                Ok(Some(change)) => changes.push(change),
                Ok(None) => {}
                Err(_) => failed += 1,
            }
        }
        if renamed > 0 {
            self.reload(false);
            self.workspace_cache_dirty |= !self.classes.contains(to);
            self.classes.insert(to);
            if let Some(scan) = &mut self.class_scan {
                scan.cached_labels.remove(to);
            }
        }
        self.status = self.language.text(
            format!(
                "Renamed {renamed} annotation(s) across {} image(s).",
                changes.len()
            ),
            format!(
                "Đã đổi nhãn cho {renamed} vùng trong {} ảnh.",
                changes.len()
            ),
        );
        if failed > 0 {
            self.status.push_str(&self.language.text(
                format!(" {failed} failed."),
                format!(" Không cập nhật được {failed} ảnh."),
            ));
        }
        self.record_file_action(changes, None);
    }

    fn delete_files(&mut self, ctx: &egui::Context, names: &[String], annotations_only: bool) {
        let active_image = self.frame.as_ref().map(|frame| frame.name.clone());
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let mut changes = Vec::new();
        let mut failed = Vec::new();
        let mut last_error = None;
        for name in names {
            let result = if annotations_only {
                workspace.archive_annotation(name)
            } else {
                workspace.archive_image(name).map(Some)
            };
            match result {
                Ok(Some(path)) => changes.push(FileChange::Archived {
                    name: name.clone(),
                    path,
                    annotations_only,
                }),
                Ok(None) => {}
                Err(error) => {
                    failed.push(name.clone());
                    last_error = Some(format!("{name}: {error:#}"));
                }
            }
        }
        if annotations_only {
            self.reload(false);
        } else {
            self.sync_file_inventory(ctx, &failed, &[]);
        }
        let what = if annotations_only {
            self.language.text("annotation file(s)", "tệp nhãn")
        } else {
            self.language.text("image(s)", "ảnh")
        };
        self.status = self.language.text(
            format!("Moved {} {what} to .rustlabel/trash.", changes.len()),
            format!("Đã chuyển {} {what} vào .rustlabel/trash.", changes.len()),
        );
        if let Some(error) = last_error {
            self.status.push_str(&self.language.text(
                format!(" {} failed. {error}", failed.len()),
                format!(" Không xóa được {} mục. {error}", failed.len()),
            ));
        }
        self.record_file_action(changes, active_image);
    }

    fn record_file_action(&mut self, changes: Vec<FileChange>, active_image: Option<String>) {
        if !changes.is_empty() {
            self.file_undo.push(FileUndo {
                changes,
                active_image,
            });
        }
    }

    fn undo_file_action(&mut self, ctx: &egui::Context) {
        let Some(batch) = self.file_undo.pop() else {
            self.status = self
                .language
                .text(
                    "No file action to undo.",
                    "Chưa có thao tác tệp để hoàn tác.",
                )
                .into();
            return;
        };
        let workspace = self
            .workspace
            .as_mut()
            .expect("file actions require a workspace");
        let mut selected = Vec::new();
        let mut moved = Vec::new();
        let mut remaining = Vec::new();
        let mut last_error = None;
        let mut restored = 0;
        for change in batch.changes.into_iter().rev() {
            let result = match &change {
                FileChange::Archived {
                    name,
                    path,
                    annotations_only,
                } => workspace
                    .restore_archive(name, path, *annotations_only)
                    .map(|()| {
                        selected.push(name.clone());
                    }),
                FileChange::Copied(name) => workspace.archive_image(name).map(|_| ()),
                FileChange::Moved { name, original } => (|| {
                    let target = workspace.root().join(original);
                    let folder = target.parent().unwrap();
                    let filename = target.file_name().and_then(|name| name.to_str()).unwrap();
                    let original = workspace.transfer_image(name, folder, Some(filename), true)?;
                    selected.push(original.clone());
                    moved.push((name.clone(), original));
                    Ok(())
                })(),
                FileChange::RenamedClass {
                    name,
                    revision,
                    indices,
                    previous,
                } => (|| {
                    let mut snapshot = workspace.inspect(name)?;
                    anyhow::ensure!(
                        snapshot.revision == *revision,
                        "Annotations changed after this file action"
                    );
                    let items = shapes_mut(&mut snapshot.doc)?;
                    for &index in indices {
                        items[index]["label"] = previous.clone().into();
                    }
                    workspace.save(name, revision, &snapshot.doc)?;
                    selected.push(name.clone());
                    Ok(())
                })(),
            };
            if let Err(error) = result {
                last_error = Some(format!("{error:#}"));
                remaining.push(change);
            } else {
                restored += 1;
            }
        }
        if restored > 0 {
            self.sync_file_inventory(ctx, &selected, &moved);
            let resume = batch
                .active_image
                .as_ref()
                .filter(|name| selected.contains(name));
            if let Some(name) =
                resume.or_else(|| self.frame.is_none().then(|| selected.first()).flatten())
                && self.frame.as_ref().is_none_or(|frame| frame.name != *name)
                && let Ok(index) = self.images.binary_search(name)
            {
                self.load(ctx, index);
            } else {
                self.reload(false);
            }
            self.focus_file_panel(ctx);
        }
        self.status = self.language.text(
            format!("Undid the file action for {restored} image(s)."),
            format!("Đã hoàn tác thao tác tệp cho {restored} ảnh."),
        );
        if let Some(error) = last_error {
            self.status.push_str(&self.language.text(
                format!(" {} failed. {error}", remaining.len()),
                format!(" Không hoàn tác được {} mục. {error}", remaining.len()),
            ));
            remaining.reverse();
            self.record_file_action(remaining, batch.active_image);
        }
    }
}
