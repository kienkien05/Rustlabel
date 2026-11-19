#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod canvas_edit;
mod class_cache;
mod class_colors;
mod file_panel;
mod ui_language;

use canvas_edit::{
    ClickCycle, Drag, Edit, HANDLES, handle, handles_at, hit_order, read_points, tag_bounds,
};
use class_cache::{ClassCache, WorkspaceCache};
use class_colors::ClassColors;
use file_panel::{FileClipboard, FileDialog, FileUndo};
use ui_language::{Language, help_ui, install_ui_fonts};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2};
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use rustlabel::formats::{self, ExportFormat};
use rustlabel::{
    IMAGE_EXTENSIONS, Point, Snapshot, Workspace, preview, shape, shapes, shapes_mut,
    validate_label,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const SELECTED: Color32 = Color32::from_rgb(209, 173, 110);
const LOGO_PNG: &[u8] = include_bytes!("../assets/rustlabel-logo.png");

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum SidebarTab {
    Image,
    Annotations,
    Classes,
    Files,
    Help,
    // Kept for deserializing layouts saved before the Files context menu.
    BatchEdit,
}

fn default_dock() -> DockState<SidebarTab> {
    let mut dock = DockState::new(vec![SidebarTab::Image]);
    let [_, inspector] = dock.main_surface_mut().split_right(
        NodeIndex::root(),
        0.76,
        vec![SidebarTab::Annotations, SidebarTab::Classes, SidebarTab::Help],
    );
    dock.main_surface_mut()
        .split_below(inspector, 0.64, vec![SidebarTab::Files]);
    dock
}

fn add_help_tab(dock: &mut DockState<SidebarTab>) {
    if dock.find_tab(&SidebarTab::Help).is_some() {
        return;
    }
    if let Some((surface, node, _)) = dock.find_tab(&SidebarTab::Annotations) {
        if let egui_dock::Node::Leaf { active, .. } = &dock[surface][node] {
            let active = *active;
            dock[surface][node].append_tab(SidebarTab::Help);
            dock.set_active_tab((surface, node, active));
        }
    } else {
        dock.push_to_first_leaf(SidebarTab::Help);
    }
}

struct SidebarViewer<F>(F, Option<egui::Id>, Language);

impl<F: FnMut(&mut egui::Ui, SidebarTab)> TabViewer for SidebarViewer<F> {
    type Tab = SidebarTab;

    fn title(&mut self, tab: &mut SidebarTab) -> egui::WidgetText {
        match tab {
            SidebarTab::Image => self.2.text("Image", "Ảnh"),
            SidebarTab::Annotations => self.2.text("Annotations", "Vùng nhãn"),
            SidebarTab::Classes => self.2.text("Classes", "Lớp"),
            SidebarTab::Files => self.2.text("Files", "Tệp"),
            SidebarTab::Help => self.2.text("Help", "Trợ giúp"),
            SidebarTab::BatchEdit => "",
        }
        .into()
    }

    fn id(&mut self, tab: &mut SidebarTab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut SidebarTab) {
        (self.0)(ui, *tab);
    }

    fn on_tab_button(&mut self, tab: &mut SidebarTab, response: &egui::Response) {
        if *tab == SidebarTab::Files && response.clicked() {
            response.request_focus();
            self.1 = Some(response.id);
        }
    }

    fn allowed_in_windows(&self, tab: &mut SidebarTab) -> bool {
        *tab != SidebarTab::Image
    }

    fn closeable(&mut self, _tab: &mut SidebarTab) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &SidebarTab) -> [bool; 2] {
        [false, false]
    }
}

fn color_swatch(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(10., ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        Rect::from_center_size(rect.center(), vec2(9., 9.)),
        2.,
        color,
    );
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Mode {
    #[default]
    Select,
    Box,
    Polygon,
    Point,
    Circle,
    Line,
    LineStrip,
}

impl Mode {
    const TOOLS: [(Self, &'static str, &'static str, Key); 7] = [
        (Self::Select, "Select (1)", "Chọn (1)", Key::Num1),
        (Self::Box, "Rectangle (2)", "Hình chữ nhật (2)", Key::Num2),
        (Self::Polygon, "Polygon (3)", "Đa giác (3)", Key::Num3),
        (Self::Circle, "Circle (4)", "Hình tròn (4)", Key::Num4),
        (Self::Line, "Line (L)", "Đoạn thẳng (L)", Key::L),
        (Self::LineStrip, "Polyline (G)", "Đường gấp khúc (G)", Key::G),
        (Self::Point, "Point (O)", "Điểm (O)", Key::O),
    ];

    fn drag_kind(self) -> Option<&'static str> {
        match self {
            Self::Box => Some("rectangle"),
            Self::Circle => Some("circle"),
            Self::Line => Some("line"),
            _ => None,
        }
    }
}

enum Icon {
    Previous,
    Next,
    Undo,
    Redo,
    Reload,
    Tool(Mode, bool),
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, hint: &str) -> egui::Response {
    let button = match &icon {
        Icon::Tool(_, selected) => egui::Button::new("")
            .min_size(vec2(32., 30.))
            .selected(*selected),
        _ => egui::Button::new("").min_size(vec2(26., 22.)),
    };
    let response = ui.add(button);
    let stroke = ui.style().interact(&response).fg_stroke;
    let origin = response.rect.center();
    let draw = |points: &[(f32, f32)]| {
        ui.painter().add(egui::Shape::line(
            points
                .iter()
                .map(|&(x, y)| origin + vec2(if matches!(icon, Icon::Redo) { -x } else { x }, y))
                .collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Previous => draw(&[(2., -5.), (-3., 0.), (2., 5.)]),
        Icon::Next => draw(&[(-2., -5.), (3., 0.), (-2., 5.)]),
        Icon::Undo | Icon::Redo => {
            draw(&[(-2., -6.), (-6., -2.), (-2., 2.)]);
            draw(&[
                (-6., -2.),
                (2., -2.),
                (5., 0.),
                (5., 3.),
                (2., 5.),
                (-3., 5.),
            ]);
        }
        Icon::Reload => {
            draw(&[
                (5., -2.),
                (2., -5.),
                (-2., -5.),
                (-5., -2.),
                (-5., 2.),
                (-2., 5.),
                (2., 5.),
                (5., 2.),
            ]);
            draw(&[(0., -2.), (5., -2.), (5., -6.)]);
        }
        Icon::Tool(mode, _) => match mode {
            Mode::Select => draw(&[
                (-5., -7.),
                (-5., 7.),
                (-1., 3.),
                (3., 7.),
                (5., 5.),
                (1., 1.),
                (7., 0.),
                (-5., -7.),
            ]),
            Mode::Box => draw(&[(-7., -5.), (7., -5.), (7., 5.), (-7., 5.), (-7., -5.)]),
            Mode::Polygon => draw(&[
                (-6., 3.),
                (-3., -6.),
                (3., -6.),
                (7., 2.),
                (1., 7.),
                (-6., 3.),
            ]),
            Mode::Circle => {
                ui.painter().circle_stroke(origin, 6., stroke);
            }
            Mode::Line => draw(&[(-6., 5.), (6., -5.)]),
            Mode::LineStrip => draw(&[(-7., 4.), (-3., -4.), (2., 3.), (7., -5.)]),
            Mode::Point => {
                ui.painter().circle_filled(origin, 3., stroke.color);
            }
        },
    }
    response.on_hover_text(hint)
}

fn paint_shape(painter: &egui::Painter, kind: &str, points: &[Pos2], stroke: Stroke) {
    match (kind, points) {
        ("rectangle", [a, b]) => {
            painter.rect_stroke(Rect::from_two_pos(*a, *b), 0., stroke, StrokeKind::Middle);
        }
        ("circle", [a, b]) => {
            painter.circle_stroke(*a, a.distance(*b), stroke);
        }
        (_, [point]) => {
            painter.circle_filled(*point, 2.5 + stroke.width, stroke.color);
        }
        ("polygon", _) => {
            painter.add(egui::Shape::closed_line(points.to_vec(), stroke));
        }
        _ => {
            painter.add(egui::Shape::line(points.to_vec(), stroke));
        }
    }
}

struct FrameImage {
    name: String,
    texture: egui::TextureHandle,
    snapshot: Snapshot,
    dirty: bool,
    undo: Vec<Vec<Value>>,
    redo: Vec<Vec<Value>>,
    hidden: BTreeSet<usize>,
    selected: Option<usize>,
}

type Scan = mpsc::Receiver<anyhow::Result<OpenedWorkspace>>;
type FormatJob = mpsc::Receiver<anyhow::Result<(PathBuf, bool)>>;

enum WorkspaceSource {
    Folder(PathBuf),
    Files(Vec<PathBuf>),
    RestoreFiles(PathBuf, Vec<String>),
}

struct OpenedWorkspace {
    workspace: Workspace,
    images: Vec<String>,
    classes: BTreeSet<String>,
    cached_skipped: Option<usize>,
    cached_next_image: Option<usize>,
}

fn open_workspace(
    source: WorkspaceSource,
    session_path: Option<&std::path::Path>,
    refresh: bool,
) -> anyhow::Result<OpenedWorkspace> {
    let (mut workspace, requested_files) = match source {
        WorkspaceSource::Folder(path) => (Workspace::open(&path)?, None),
        WorkspaceSource::Files(paths) => (Workspace::open_files(&paths)?, None),
        WorkspaceSource::RestoreFiles(root, names) => (Workspace::open(&root)?, Some(names)),
    };
    let selected_files = requested_files.is_some() || workspace.is_file_selection();
    if !refresh
        && let Some(cache) = WorkspaceCache::load(session_path, workspace.root(), selected_files)
        && cache.next_image.is_none_or(|next| next <= cache.images.len())
        && requested_files.as_ref().is_none_or(|names| *names == cache.images)
        && (!workspace.is_file_selection()
            || workspace.images().is_ok_and(|names| names == cache.images))
        && let Ok(restored) = workspace
            .clone()
            .with_cached_inventory(cache.images.clone(), selected_files)
    {
        return Ok(OpenedWorkspace {
            workspace: restored,
            images: cache.images,
            classes: cache.classes,
            cached_skipped: Some(cache.skipped),
            cached_next_image: cache.next_image,
        });
    }
    if let Some(names) = requested_files {
        workspace = Workspace::restore_file_selection(workspace.root(), &names)?;
    }
    let images = workspace.images()?.to_vec();
    let classes = if selected_files {
        BTreeSet::new()
    } else {
        workspace_classes(&workspace)
    };
    Ok(OpenedWorkspace {
        workspace,
        images,
        classes,
        cached_skipped: None,
        cached_next_image: None,
    })
}

fn output_folder(language: Language) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(language.text("Choose output parent on the same drive", "Chọn thư mục lưu trên cùng ổ đĩa"))
        .pick_folder()
}

fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn();
        }
    }
}

fn clean_path(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct SessionCache {
    #[serde(default)]
    language: Language,
    #[serde(default)]
    last_workspace_folder: Option<PathBuf>,
    #[serde(default)]
    last_image_name: Option<String>,
    #[serde(default)]
    last_file_selection: Option<Vec<String>>,
    #[serde(default)]
    view: SessionView,
    #[serde(default)]
    recent_workspaces: Vec<PathBuf>,
    #[serde(default)]
    workspace_last_images: std::collections::HashMap<PathBuf, String>,
    #[serde(default)]
    last_label: Option<String>,
    #[serde(default)]
    last_mode: Option<Mode>,
    #[serde(default)]
    show_shapes: Option<bool>,
    #[serde(default)]
    show_names: Option<bool>,
    #[serde(default)]
    sidebar_dock: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SessionView {
    filter: String,
    selected_files: Vec<String>,
    canvas: Option<(f32, [f32; 2])>,
}

#[cfg(test)]
fn global_session_cache_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("RUSTLABEL_SESSION_CACHE") {
        return Some(PathBuf::from(custom));
    }
    None
}

#[cfg(not(test))]
fn global_session_cache_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("RUSTLABEL_SESSION_CACHE") {
        return Some(PathBuf::from(custom));
    }
    // Check if session.json exists beside the executable (portable mode)
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop();
        let local_session = exe.join("session.json");
        if local_session.is_file() {
            return Some(local_session);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let dir = PathBuf::from(appdata).join("RustLabel");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("session.json"));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
            let dir = PathBuf::from(config_home).join("rustlabel");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("session.json"));
        }
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(home).join(".config").join("rustlabel");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("session.json"));
        }
    }
    None
}

struct ClassScan {
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    result: mpsc::Receiver<ClassScanUpdate>,
    workspace: Workspace,
    scanned: usize,
    total: usize,
    cached_labels: BTreeSet<String>,
    warm: bool,
}

#[derive(Default)]
struct ClassScanUpdate {
    labels: BTreeSet<String>,
    scanned: usize,
    skipped: usize,
    finished: bool,
    preview: bool,
    cache_error: bool,
    checkpoint: bool,
}

impl Drop for ClassScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum LabelTarget {
    New {
        kind: &'static str,
        points: Vec<Point>,
    },
    Existing(usize),
}

struct LabelDraft {
    target: LabelTarget,
    input: String,
    error: String,
    focus: bool,
}

fn workspace_classes(workspace: &Workspace) -> BTreeSet<String> {
    let Ok(file) = File::open(workspace.root().join("labels.txt")) else {
        return BTreeSet::new();
    };
    let mut classes = String::new();
    if file
        .take(256 * 1024 + 1)
        .read_to_string(&mut classes)
        .is_err()
        || classes.len() > 256 * 1024
    {
        return BTreeSet::new();
    }
    classes
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|label| !label.is_empty() && *label != "__ignore__")
        .map(str::to_owned)
        .collect()
}

fn remember_classes(classes: &mut ClassColors, doc: &Value) -> bool {
    let before = classes.len();
    if let Ok(items) = shapes(doc) {
        for label in items.iter().filter_map(|item| item["label"].as_str()) {
            classes.insert(label);
        }
    }
    classes.len() != before
}

struct App {
    language: Language,
    brand_mark: Option<egui::TextureHandle>,
    workspace: Option<Workspace>,
    scan: Option<Scan>,
    refreshing_workspace: bool,
    format_job: Option<FormatJob>,
    images: Vec<String>,
    filtered: Vec<usize>,
    filter: String,
    frame: Option<FrameImage>,
    label: String,
    classes: ClassColors,
    class_scan: Option<ClassScan>,
    class_scan_errors: usize,
    class_cache_error: bool,
    workspace_cache_ready: bool,
    workspace_cache_dirty: bool,
    workspace_cache_next_image: Option<usize>,
    sidebar_dock: Option<DockState<SidebarTab>>,
    label_draft: Option<LabelDraft>,
    mode: Mode,
    zoom: f32,
    zoom_needs_input: bool,
    pan: Vec2,
    start: Option<Pos2>,
    annotation_drag: Option<Drag>,
    click_cycle: ClickCycle,
    draft_points: Vec<Point>,
    show_shapes: bool,
    show_names: bool,
    status: String,
    refresh_at: Instant,
    discard: bool,
    closing: bool,
    selected_files: BTreeSet<usize>,
    last_file_click: Option<usize>,
    file_focus: Option<egui::Id>,
    file_panel_focus: Option<egui::Id>,
    restore_files_focus: bool,
    centered_file_row: Option<(usize, usize, f32)>,
    file_drag_anchor: Option<usize>,
    file_undo: Vec<FileUndo>,
    file_clipboard: Option<FileClipboard>,
    file_dialog: Option<FileDialog>,
    tag_rects: Vec<(Rect, usize)>,
    recent_workspaces: Vec<PathBuf>,
    workspace_last_images: std::collections::HashMap<PathBuf, String>,
    target_resume_image: Option<String>,
    target_resume_view: Option<SessionView>,
    cached_session: Option<SessionCache>,
    session_cache_path_override: Option<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            language: Language::default(),
            brand_mark: None,
            workspace: None,
            scan: None,
            refreshing_workspace: false,
            format_job: None,
            images: Vec::new(),
            filtered: Vec::new(),
            filter: String::new(),
            frame: None,
            label: String::new(),
            classes: ClassColors::default(),
            class_scan: None,
            class_scan_errors: 0,
            class_cache_error: false,
            workspace_cache_ready: false,
            workspace_cache_dirty: false,
            workspace_cache_next_image: None,
            sidebar_dock: Some(default_dock()),
            label_draft: None,
            mode: Mode::default(),
            zoom: 1.,
            zoom_needs_input: false,
            pan: Vec2::ZERO,
            start: None,
            annotation_drag: None,
            click_cycle: ClickCycle::default(),
            draft_points: Vec::new(),
            show_shapes: true,
            show_names: true,
            status: String::new(),
            refresh_at: Instant::now(),
            discard: false,
            closing: false,
            selected_files: BTreeSet::new(),
            last_file_click: None,
            file_focus: None,
            file_panel_focus: None,
            restore_files_focus: false,
            centered_file_row: None,
            file_drag_anchor: None,
            file_undo: Vec::new(),
            file_clipboard: None,
            file_dialog: None,
            tag_rects: Vec::new(),
            recent_workspaces: Vec::new(),
            workspace_last_images: std::collections::HashMap::new(),
            target_resume_image: None,
            target_resume_view: None,
            cached_session: None,
            session_cache_path_override: None,
        }
    }
}

impl App {
    fn add_recent_workspace(&mut self, path: &std::path::Path) {
        let resolved = path
            .canonicalize()
            .map(|p| clean_path(&p))
            .unwrap_or_else(|_| clean_path(path));
        self.recent_workspaces.retain(|p| {
            p != &resolved
                && p.canonicalize()
                    .map(|cp| clean_path(&cp))
                    .ok()
                    .as_ref()
                    != Some(&resolved)
        });
        self.recent_workspaces.insert(0, resolved);
        if self.recent_workspaces.len() > 10 {
            self.recent_workspaces.truncate(10);
        }
    }

    fn cache_path(&self) -> Option<PathBuf> {
        self.session_cache_path_override
            .clone()
            .or_else(global_session_cache_path)
    }

    fn save_workspace_cache(&mut self) {
        if !self.workspace_cache_ready || !std::mem::take(&mut self.workspace_cache_dirty) {
            return;
        }
        if let (Some(workspace), Some(path)) = (&self.workspace, self.cache_path()) {
            let cache = WorkspaceCache {
                images: self.images.clone(),
                classes: self
                    .classes
                    .labels()
                    .filter(|label| self.class_scan.as_ref().is_none_or(|scan| !scan.cached_labels.contains(*label)))
                    .cloned()
                    .collect(),
                skipped: self.class_scan_errors,
                next_image: self.workspace_cache_next_image,
            };
            self.class_cache_error |= cache.save(&path, workspace).is_err();
        }
    }

    fn save_session_cache(&mut self) {
        if self.scan.is_some() || self.sidebar_dock.is_none() {
            return;
        }
        let workspace_folder = self.workspace.as_ref().map(|w| clean_path(w.root()));
        let file_selection = self.workspace.as_ref().is_some_and(Workspace::is_file_selection);

        let current_image = self.frame.as_ref().map(|f| f.name.clone());

        if !file_selection && let (Some(folder), Some(image)) = (&workspace_folder, &current_image) {
            self.workspace_last_images.insert(folder.clone(), image.clone());
        }

        let dock_value = self
            .sidebar_dock
            .as_ref()
            .and_then(|dock| serde_json::to_value(dock).ok());

        if let Some(path) = self.cache_path() {
            let session = SessionCache {
                language: self.language,
                last_workspace_folder: workspace_folder
                    .or_else(|| self.recent_workspaces.first().cloned()),
                last_image_name: current_image,
                last_file_selection: file_selection.then(|| self.images.clone()),
                view: SessionView {
                    filter: self.filter.clone(),
                    selected_files: self.selected_file_names(),
                    canvas: self.frame.as_ref().map(|_| (self.zoom, [self.pan.x, self.pan.y])),
                },
                recent_workspaces: self.recent_workspaces.clone(),
                workspace_last_images: self.workspace_last_images.clone(),
                last_label: if self.label.is_empty() {
                    None
                } else {
                    Some(self.label.clone())
                },
                last_mode: Some(self.mode),
                show_shapes: Some(self.show_shapes),
                show_names: Some(self.show_names),
                sidebar_dock: dock_value,
            };
            if let Ok(json) = serde_json::to_string_pretty(&session)
                && let Some(folder) = path.parent()
                && let Ok(mut temp) = tempfile::NamedTempFile::new_in(folder)
                && temp.write_all(json.as_bytes()).is_ok()
            {
                let _ = temp.persist(path);
            }
        }
    }

    fn load_session_cache(&mut self) {
        let Some(path) = self.cache_path() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(mut session) = serde_json::from_str::<SessionCache>(&content) else {
            return;
        };
        session.recent_workspaces.retain(|p| p.is_dir());
        self.recent_workspaces = session.recent_workspaces.clone();
        self.workspace_last_images = session.workspace_last_images.clone();
        self.language = session.language;
        if let Some(show) = session.show_shapes {
            self.show_shapes = show;
        }
        if let Some(show) = session.show_names {
            self.show_names = show;
        }
        if let Some(mode) = session.last_mode {
            self.mode = mode;
        }
        if let Some(ref label) = session.last_label {
            if !label.trim().is_empty() {
                self.label = label.trim().to_owned();
            }
        }
        if let Some(val) = session.sidebar_dock.clone() {
            if let Ok(mut dock) = serde_json::from_value::<DockState<SidebarTab>>(val) {
                while let Some(tab) = dock.find_tab(&SidebarTab::BatchEdit) {
                    dock.remove_tab(tab);
                }
                add_help_tab(&mut dock);
                self.sidebar_dock = Some(dock);
            }
        }
        self.cached_session = Some(session);
    }

    fn restore_last_session(&mut self, ctx: &egui::Context) -> bool {
        let Some(session) = self.cached_session.take() else {
            return false;
        };
        let Some(folder) = session.last_workspace_folder.filter(|folder| folder.is_dir()) else {
            return false;
        };
        self.target_resume_image = session.last_image_name;
        self.target_resume_view = Some(session.view);
        if let Some(names) = session.last_file_selection {
            self.open_source(ctx, WorkspaceSource::RestoreFiles(folder, names), false);
        } else {
            self.open(ctx, folder);
        }
        true
    }

    fn run_format_job(
        &mut self,
        ctx: &egui::Context,
        import: bool,
        run: impl FnOnce() -> anyhow::Result<PathBuf> + Send + 'static,
    ) {
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.format_job = Some(rx);
        self.status = if import {
            self.language.text("Importing dataset...", "Đang nhập bộ dữ liệu…")
        } else {
            self.language.text("Exporting dataset...", "Đang xuất bộ dữ liệu…")
        }
        .into();
        std::thread::spawn(move || {
            let _ = tx.send(run().map(|path| (path, import)));
            ctx.request_repaint();
        });
    }

    fn format_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.language;
        ui.add_enabled_ui(
            self.format_job.is_none() && self.scan.is_none() && !self.pending(),
            |ui| {
                ui.menu_button(lang.text("Dataset", "Bộ dữ liệu"), |ui| {
                    if ui.button(lang.text("Import COCO", "Nhập COCO")).clicked() {
                        ui.close_menu();
                        if let Some(manifest) = rfd::FileDialog::new()
                            .add_filter("COCO JSON", &["json"])
                            .pick_file()
                            && let Some(images) = rfd::FileDialog::new()
                                .set_title(lang.text("Choose COCO image root", "Chọn thư mục gốc chứa ảnh COCO"))
                                .pick_folder()
                            && let Some(parent) = output_folder(lang)
                        {
                            self.run_format_job(ctx, true, move || {
                                formats::import_coco(&manifest, &images, &parent)
                            });
                        }
                    }
                    if ui.button(lang.text("Import YOLO", "Nhập YOLO")).clicked() {
                        ui.close_menu();
                        if let Some(dataset) = rfd::FileDialog::new()
                            .set_title(lang.text("Choose YOLO dataset folder", "Chọn thư mục dữ liệu YOLO"))
                            .pick_folder()
                            && let Some(parent) = output_folder(lang)
                        {
                            self.run_format_job(ctx, true, move || {
                                formats::import_yolo(&dataset, &parent)
                            });
                        }
                    }
                    ui.separator();
                    for (format, name) in [
                        (ExportFormat::Coco, lang.text("Export COCO", "Xuất COCO")),
                        (ExportFormat::YoloBoxes, lang.text("Export YOLO Detection", "Xuất YOLO nhận diện")),
                        (ExportFormat::YoloPolygons, lang.text("Export YOLO Segmentation", "Xuất YOLO phân đoạn")),
                    ] {
                        if ui
                            .add_enabled(self.workspace.is_some(), egui::Button::new(name))
                            .clicked()
                        {
                            ui.close_menu();
                            if let Some(workspace) = self.workspace.clone()
                                && let Some(parent) = output_folder(lang)
                            {
                                let classes = self.classes.labels().cloned().collect();
                                self.run_format_job(ctx, false, move || {
                                    formats::export_dataset(&workspace, &parent, format, classes)
                                });
                            }
                        }
                    }
                });
            },
        );
    }

    fn scan_classes(
        &mut self,
        workspace: Workspace,
        ctx: &egui::Context,
        start: usize,
        skipped: usize,
    ) {
        self.class_scan_errors = skipped;
        self.class_cache_error = false;
        let total = workspace.images().map_or(0, <[String]>::len);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, result) = mpsc::channel();
        let ctx = ctx.clone();
        let cache_path = self.cache_path();
        let scan_workspace = workspace.clone();
        let worker = std::thread::spawn(move || {
            let workspace = scan_workspace;
            let mut cache = ClassCache::load(cache_path.as_deref(), &workspace);
            let mut labels = BTreeSet::new();
            let mut skipped = skipped;
            let mut scanned = start;
            let mut cache_error = false;
            let mut last_update = Instant::now();
            let mut last_checkpoint = last_update;
            if let Ok(images) = workspace.images() {
                let preview = (start == 0)
                    .then(|| cache.preview_labels(images))
                    .filter(|labels| !labels.is_empty());
                if let Some(preview) = preview {
                    if tx
                        .send(ClassScanUpdate {
                            labels: preview,
                            preview: true,
                            ..Default::default()
                        })
                        .is_err()
                    {
                        return;
                    }
                    ctx.request_repaint();
                }
                for (index, image) in images.iter().enumerate().skip(start) {
                    if worker_cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if cache.collect(&workspace, image, &mut labels).is_err() {
                        skipped += 1;
                    }
                    scanned = index + 1;
                    if last_update.elapsed() >= Duration::from_millis(200) {
                        let checkpoint = last_checkpoint.elapsed() >= Duration::from_secs(2);
                        if checkpoint {
                            last_checkpoint = Instant::now();
                        }
                        if tx
                            .send(ClassScanUpdate {
                                labels: std::mem::take(&mut labels),
                                scanned,
                                skipped,
                                checkpoint,
                                ..Default::default()
                            })
                            .is_err()
                        {
                            return;
                        }
                        ctx.request_repaint();
                        last_update = Instant::now();
                    }
                }
                cache_error = cache.save(&workspace, images).is_err();
            }
            let _ = tx.send(ClassScanUpdate {
                labels,
                scanned,
                skipped,
                finished: true,
                cache_error,
                ..Default::default()
            });
            ctx.request_repaint();
        });
        self.class_scan = Some(ClassScan {
            cancel,
            worker: Some(worker),
            result,
            workspace,
            scanned: start,
            total,
            cached_labels: BTreeSet::new(),
            warm: false,
        });
    }

    fn poll_class_scan(&mut self) {
        while let Some(scan) = &mut self.class_scan {
            match scan.result.try_recv() {
                Ok(update) if update.preview => {
                    scan.warm = true;
                    for label in update.labels {
                        if !self.classes.contains(&label) {
                            self.classes.insert(&label);
                            scan.cached_labels.insert(label);
                        }
                    }
                }
                Ok(update) => {
                    for label in update.labels {
                        scan.cached_labels.remove(&label);
                        self.classes.insert(&label);
                    }
                    self.class_scan_errors = update.skipped;
                    scan.scanned = update.scanned;
                    if update.checkpoint || update.finished {
                        let verified = scan.workspace.images().map_or(0, |images| {
                            self.images
                                .iter()
                                .zip(images.iter().take(update.scanned))
                                .take_while(|(current, scanned)| current == scanned)
                                .count()
                        });
                        self.workspace_cache_next_image =
                            (verified < self.images.len()).then_some(verified);
                        self.workspace_cache_dirty = true;
                    }
                    if update.finished {
                        for label in &scan.cached_labels {
                            self.classes.remove(label);
                        }
                        if let Some(frame) = &self.frame {
                            remember_classes(&mut self.classes, &frame.snapshot.doc);
                        }
                        self.class_cache_error = update.cache_error;
                        self.class_scan = None;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.class_scan_errors += 1;
                    self.class_scan = None;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
    }

    fn save_partial_class_scan(&mut self) {
        if let Some(scan) = &mut self.class_scan {
            scan.cancel.store(true, Ordering::Relaxed);
            if let Some(worker) = scan.worker.take() {
                let _ = worker.join();
            }
        }
        self.poll_class_scan();
        self.save_workspace_cache();
    }

    fn pending(&self) -> bool {
        self.frame.as_ref().is_some_and(|f| f.dirty)
            || !self.draft_points.is_empty()
            || self.label_draft.is_some()
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.scan.is_some() || self.format_job.is_some() {
            return;
        }
        if self.pending() {
            self.status = self.language.text("Save or discard your current work first.", "Hãy lưu hoặc bỏ chỉnh sửa hiện tại trước.").into();
            return;
        }
        let clean = clean_path(&path);
        self.add_recent_workspace(&clean);
        if self.target_resume_image.is_none() {
            if let Some(img) = self.workspace_last_images.get(&clean) {
                self.target_resume_image = Some(img.clone());
            }
        }
        self.open_source(ctx, WorkspaceSource::Folder(path), false);
    }

    fn open_source(&mut self, ctx: &egui::Context, source: WorkspaceSource, refresh: bool) {
        if self.scan.is_some() || self.format_job.is_some() {
            return;
        }
        if self.pending() {
            self.status = self.language.text("Save or discard your current work first.", "Hãy lưu hoặc bỏ chỉnh sửa hiện tại trước.").into();
            return;
        }
        self.save_partial_class_scan();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let session_path = self.cache_path();
        std::thread::spawn(move || {
            let result = open_workspace(source, session_path.as_deref(), refresh);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.scan = Some(rx);
        self.refreshing_workspace = refresh;
        self.status = if refresh {
            self.language.text("Refreshing files and classes...", "Đang cập nhật tệp và lớp…")
        } else {
            self.language.text("Opening workspace...", "Đang mở bộ ảnh…")
        }
        .into();
    }

    fn refresh_workspace(&mut self, ctx: &egui::Context) {
        if self.scan.is_some() || self.format_job.is_some() || self.pending() {
            return;
        }
        let Some(workspace) = &self.workspace else { return };
        let root = workspace.root().to_owned();
        let source = if workspace.is_file_selection() {
            WorkspaceSource::RestoreFiles(root, self.images.clone())
        } else {
            WorkspaceSource::Folder(root)
        };
        self.open_source(ctx, source, true);
    }

    fn finish_workspace_scan(&mut self, ctx: &egui::Context, opened: OpenedWorkspace) {
        let OpenedWorkspace {
            workspace,
            images,
            classes,
            cached_skipped,
            cached_next_image,
        } = opened;
        self.scan = None;
        self.workspace_cache_ready = true;
        self.workspace_cache_dirty = cached_skipped.is_none();
        self.workspace_cache_next_image = if cached_skipped.is_some() {
            cached_next_image
        } else {
            Some(0)
        };
        if let Some(skipped) = cached_skipped {
            self.class_scan = None;
            self.class_scan_errors = skipped;
            self.class_cache_error = false;
        }
        if std::mem::take(&mut self.refreshing_workspace) {
            let selected = self.selected_file_names();
            self.workspace = Some(workspace.clone());
            self.classes = classes.into_iter().collect();
            self.sync_file_inventory(ctx, &selected, &[]);
            if self.frame.is_none() {
                self.navigate(ctx, true);
            } else {
                self.reload(false);
            }
            if let Some(frame) = &self.frame {
                remember_classes(&mut self.classes, &frame.snapshot.doc);
            }
            self.scan_classes(workspace, ctx, 0, 0);
            self.status = self.language.text(
                format!("Refreshed {} image(s).", self.images.len()),
                format!("Đã cập nhật {} ảnh.", self.images.len()),
            );
            return;
        }
        if let Some(next_image) = self.workspace_cache_next_image {
            self.scan_classes(
                workspace.clone(),
                ctx,
                next_image,
                cached_skipped.unwrap_or_default(),
            );
        }
        self.workspace = Some(workspace);
        self.images = images;
        self.selected_files.clear();
        self.last_file_click = None;
        self.file_focus = None;
        self.file_panel_focus = None;
        self.restore_files_focus = false;
        self.centered_file_row = None;
        self.file_drag_anchor = None;
        self.file_undo.clear();
        self.file_clipboard = None;
        let resume_view = self.target_resume_view.take().unwrap_or_default();
        self.filter = resume_view.filter;
        self.selected_files = resume_view.selected_files.iter()
            .filter_map(|name| self.images.binary_search(name).ok())
            .collect();
        self.last_file_click = self.selected_files.first().copied();
        self.classes = classes.into_iter().collect();
        if !self.label.is_empty() {
            self.workspace_cache_dirty |= !self.classes.contains(&self.label);
            self.classes.insert(&self.label);
        }
        self.frame = None;
        self.filter();
        self.status = if self.images.is_empty() {
            self.language.text("No supported images in this folder.", "Thư mục này không có ảnh được hỗ trợ.").into()
        } else {
            String::new()
        };
        let resume_idx = self.target_resume_image.take().and_then(|target| {
            self.images.iter().position(|name| *name == target)
        });
        if let Some(idx) = resume_idx {
            self.load(ctx, idx);
            if self.frame.is_some()
                && let Some((zoom, [x, y])) = resume_view.canvas
                && (0.25..=24.).contains(&zoom)
                && x.is_finite() && y.is_finite()
            {
                self.zoom = zoom;
                self.pan = vec2(x, y);
            }
        } else {
            self.navigate(ctx, true);
        }
        self.save_session_cache();
    }

    fn handle_file_drop(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<_> = dropped
            .iter()
            .filter_map(|file| file.path.clone())
            .collect();
        if paths.len() != dropped.len() {
            self.status = self.language.text("Drop image files from your file manager.", "Hãy kéo tệp ảnh từ trình quản lý tệp vào đây.").into();
        } else if !self.discard && self.label_draft.is_none() {
            self.open_source(ctx, WorkspaceSource::Files(paths), false);
        }
    }

    fn filter(&mut self) {
        let filter = self.filter.to_lowercase();
        self.filtered = self
            .images
            .iter()
            .enumerate()
            .filter(|(_, name)| filter.is_empty() || name.to_lowercase().contains(&filter))
            .map(|(index, _)| index)
            .collect();
    }

    fn load(&mut self, ctx: &egui::Context, index: usize) {
        if self.pending() {
            self.status = self.language.text("Save or discard your current work first.", "Hãy lưu hoặc bỏ chỉnh sửa hiện tại trước.").into();
            return;
        }
        let (Some(workspace), Some(name)) = (&self.workspace, self.images.get(index)) else {
            return;
        };
        let result = (|| {
            let path = workspace.image_path(name)?;
            let snapshot = workspace.inspect(name)?;
            let pixels = preview(&path, 4096)?.into_rgba8();
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width() as usize, pixels.height() as usize],
                &pixels,
            );
            let texture = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
            Ok::<_, anyhow::Error>(FrameImage {
                name: name.clone(),
                texture,
                snapshot,
                dirty: false,
                undo: Vec::new(),
                redo: Vec::new(),
                hidden: BTreeSet::new(),
                selected: None,
            })
        })();
        match result {
            Ok(frame) => {
                self.workspace_cache_dirty |= remember_classes(&mut self.classes, &frame.snapshot.doc);
                self.frame = Some(frame);
                self.fit_view();
                self.start = None;
                self.annotation_drag = None;
                self.click_cycle = ClickCycle::default();
                self.status.clear();
                self.save_session_cache();
            }
            Err(error) => self.status = format!("{name}: {error:#}"),
        }
    }

    fn navigate(&mut self, ctx: &egui::Context, forward: bool) {
        let current = self
            .frame
            .as_ref()
            .and_then(|f| self.images.binary_search(&f.name).ok())
            .and_then(|i| self.filtered.binary_search(&i).ok());
        let next = match current {
            None => {
                if forward {
                    self.filtered.first()
                } else {
                    self.filtered.last()
                }
            }
            Some(i) if forward => self.filtered.get(i + 1),
            Some(i) => i.checked_sub(1).and_then(|i| self.filtered.get(i)),
        };
        if let Some(&index) = next {
            self.load(ctx, index);
        }
    }

    fn navigation_shortcut(&mut self, ctx: &egui::Context) {
        let files_focused = self.files_have_focus(ctx);
        if (ctx.wants_keyboard_input() && !files_focused)
            || self.discard
            || self.label_draft.is_some()
            || self.file_dialog.is_some()
            || self.scan.is_some()
            || self.format_job.is_some()
            || self.annotation_drag.is_some()
            || self.start.is_some()
            || self.file_drag_anchor.is_some()
        {
            return;
        }
        let direction = ctx.input_mut(|input| {
            if !input.modifiers.is_none() {
                return None;
            }
            let keys = egui::Modifiers::NONE;
            if input.consume_key(keys, Key::A)
                || input.consume_key(keys, Key::ArrowLeft)
                || (files_focused && input.consume_key(keys, Key::ArrowUp))
            {
                Some(false)
            } else if input.consume_key(keys, Key::D)
                || input.consume_key(keys, Key::ArrowRight)
                || (files_focused && input.consume_key(keys, Key::ArrowDown))
            {
                Some(true)
            } else {
                None
            }
        });
        let Some(forward) = direction else { return };
        let previous = self.frame.as_ref().map(|frame| frame.name.clone());
        self.navigate(ctx, forward);
        if files_focused {
            if let Some(frame) = &self.frame
                && previous.as_ref() != Some(&frame.name)
                && let Ok(index) = self.images.binary_search(&frame.name)
            {
                self.selected_files = BTreeSet::from([index]);
                self.last_file_click = Some(index);
            }
            self.focus_file_panel(ctx);
            self.restore_files_focus = true;
        }
    }

    fn save(&mut self) {
        if self.label_draft.is_some() {
            self.status = self.language.text("Choose a label or cancel the draft before saving.", "Hãy chọn nhãn hoặc hủy bản nháp trước khi lưu.").into();
            return;
        }
        if !self.draft_points.is_empty() {
            self.status = self.language.text("Finish or cancel the shape before saving.", "Hãy hoàn tất hoặc hủy hình trước khi lưu.").into();
            return;
        }
        let (Some(workspace), Some(frame)) = (&self.workspace, &mut self.frame) else {
            return;
        };
        if !frame.dirty {
            return;
        }
        match workspace.save(&frame.name, &frame.snapshot.revision, &frame.snapshot.doc) {
            Ok(revision) => {
                frame.snapshot.revision = revision;
                frame.dirty = false;
                self.status = self.language.text("Saved", "Đã lưu").into();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
    }

    fn fit_view(&mut self) {
        self.zoom = 1.;
        self.zoom_needs_input = true;
        self.pan = Vec2::ZERO;
        self.click_cycle = ClickCycle::default();
    }

    fn fit_shortcut(&mut self, ctx: &egui::Context) {
        if !self.discard
            && self.label_draft.is_none()
            && self.file_dialog.is_none()
            && self.start.is_none()
            && self.annotation_drag.is_none()
            && ctx.input_mut(|i| {
                let m = i.modifiers;
                let modifier = if m.command {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::CTRL
                };
                !m.alt
                    && !m.shift
                    && (m.command || m.ctrl)
                    && i.consume_key(modifier, Key::F)
            })
        {
            self.fit_view();
        }
    }

    fn checkpoint(frame: &mut FrameImage) {
        if let Ok(existing) = shapes(&frame.snapshot.doc) {
            if frame.undo.len() >= 32 {
                frame.undo.remove(0);
            }
            frame.undo.push(existing.clone());
            frame.redo.clear();
            frame.dirty = true;
        }
    }

    fn queue_label(&mut self, kind: &'static str, points: Vec<Point>) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };
        if let Err(error) = shape("validated", kind, &points, frame.snapshot.size) {
            self.status = error.to_string();
            return false;
        }
        self.open_label_picker(LabelTarget::New { kind, points });
        true
    }

    fn open_label_picker(&mut self, target: LabelTarget) {
        self.label_draft = Some(LabelDraft {
            target,
            input: String::new(),
            error: String::new(),
            focus: true,
        });
        self.status.clear();
    }

    fn confirm_label(&mut self, label: &str) -> bool {
        let (Some(draft), Some(frame)) = (&mut self.label_draft, &mut self.frame) else {
            return false;
        };
        let result = match &draft.target {
            LabelTarget::New { kind, points } => shape(label, kind, points, frame.snapshot.size)
                .map(|item| {
                    Self::checkpoint(frame);
                    let items = shapes_mut(&mut frame.snapshot.doc).expect("validated shapes");
                    frame.selected = Some(items.len());
                    items.push(item);
                }),
            LabelTarget::Existing(index) => validate_label(label).and_then(|label| {
                let item = shapes(&frame.snapshot.doc)?
                    .get(*index)
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("Selected annotation is no longer available"))?;
                if item.get("label").and_then(Value::as_str) != Some(label) {
                    Self::checkpoint(frame);
                    shapes_mut(&mut frame.snapshot.doc)?[*index]["label"] = label.into();
                }
                Ok(())
            }),
        };
        match result {
            Ok(()) => {
                self.label = label.trim().to_owned();
                self.workspace_cache_dirty |= !self.classes.contains(&self.label);
                self.classes.insert(&self.label);
                if let Some(scan) = &mut self.class_scan {
                    scan.cached_labels.remove(&self.label);
                }
                self.label_draft = None;
                self.save();
                self.save_session_cache();
                true
            }
            Err(error) => {
                draft.error = error.to_string();
                draft.focus = true;
                false
            }
        }
    }

    fn finish_path(&mut self) {
        let kind = match self.mode {
            Mode::Polygon => "polygon",
            Mode::LineStrip => "linestrip",
            _ => return,
        };
        if self.queue_label(kind, self.draft_points.clone()) {
            self.draft_points.clear();
        }
    }

    fn select_tool(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.draft_points.clear();
            self.start = None;
            self.annotation_drag = None;
            self.click_cycle = ClickCycle::default();
            self.save_session_cache();
        }
    }

    fn tool_shortcut(&mut self, ctx: &egui::Context) {
        if (ctx.wants_keyboard_input() && !self.files_have_focus(ctx))
            || self.discard
            || self.label_draft.is_some()
            || self.file_dialog.is_some()
            || self.format_job.is_some()
            || self.annotation_drag.is_some()
            || self.start.is_some()
            || !self.draft_points.is_empty()
        {
            return;
        }
        let tool = ctx.input(|input| {
            Mode::TOOLS
                .iter()
                .find(|(_, _, _, key)| input.modifiers.is_none() && input.key_pressed(*key))
                .map(|(mode, _, _, _)| *mode)
        });
        if let Some(mode) = tool {
            self.select_tool(mode);
        }
    }

    fn can_relabel(&self) -> bool {
        !self.discard
            && self.label_draft.is_none()
            && self.file_dialog.is_none()
            && self.scan.is_none()
            && self.format_job.is_none()
            && self.annotation_drag.is_none()
            && self.start.is_none()
            && self.draft_points.is_empty()
    }

    fn relabel_shortcut(&mut self, ctx: &egui::Context) {
        let Some(index) = self.frame.as_ref().and_then(|frame| frame.selected) else {
            return;
        };
        if self.can_relabel()
            && !ctx.memory(|memory| memory.has_focus(egui::Id::new(file_panel::FILTER_ID)))
            && ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::COMMAND, Key::E)
                    || input.consume_key(egui::Modifiers::CTRL, Key::E)
            })
        {
            self.open_label_picker(LabelTarget::Existing(index));
        }
    }

    fn annotation_rows(&mut self, ui: &mut egui::Ui) {
        enum Action {
            Relabel,
            Delete,
        }
        let can_edit = self.can_relabel();
        let lang = self.language;
        let Some(frame) = &mut self.frame else {
            return;
        };
        let Ok(items) = shapes(&frame.snapshot.doc) else {
            return;
        };
        let mut action = None;
        ui.weak(lang.text(format!("{} shapes", items.len()), format!("{} vùng", items.len())));
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, item) in items.iter().enumerate() {
                let class = item["label"].as_str();
                let label = class.unwrap_or("?");
                let kind = lang.shape(item["shape_type"].as_str().unwrap_or("shape"));
                let row = format!("{}  {label}  ·  {kind}", index + 1);
                ui.horizontal(|ui| {
                    let mut show = !frame.hidden.contains(&index);
                    if ui
                        .checkbox(&mut show, "")
                        .on_hover_text(lang.text("Show this annotation", "Hiện vùng nhãn này"))
                        .changed()
                    {
                        if show {
                            frame.hidden.remove(&index);
                        } else {
                            frame.hidden.insert(index);
                            if frame.selected == Some(index) {
                                frame.selected = None;
                            }
                        }
                        self.click_cycle = ClickCycle::default();
                    }
                    color_swatch(ui, self.classes.color(label));
                    let button = ui
                        .add(
                            egui::Button::new(&row)
                                .selected(frame.selected == Some(index))
                                .truncate(),
                        )
                        .on_hover_text(&row);
                    button.context_menu(|ui| {
                        let shortcut = if cfg!(target_os = "macos") {
                            "Cmd+E"
                        } else {
                            "Ctrl+E"
                        };
                        if ui
                            .add_enabled(
                                can_edit && item.is_object(),
                                egui::Button::new(lang.text("Change label…", "Đổi nhãn…")).shortcut_text(shortcut),
                            )
                            .clicked()
                        {
                            action = Some((index, Action::Relabel));
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(class.is_some(), egui::Button::new(lang.text("Copy label", "Chép tên nhãn")))
                            .clicked()
                            && let Some(class) = class
                        {
                            ui.ctx().copy_text(class.to_owned());
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(can_edit, egui::Button::new(lang.text("Delete", "Xóa")))
                            .clicked()
                        {
                            action = Some((index, Action::Delete));
                            ui.close_menu();
                        }
                    });
                    if button.clicked()
                        || button.secondary_clicked()
                        || action.as_ref().is_some_and(|(row, _)| *row == index)
                    {
                        frame.selected = Some(index);
                        self.click_cycle = ClickCycle::default();
                        self.label = label.into();
                    }
                });
            }
        });
        match action {
            Some((index, Action::Relabel)) => self.open_label_picker(LabelTarget::Existing(index)),
            Some((_, Action::Delete)) => self.delete_selected(),
            None => {}
        }
    }

    fn label_picker(&mut self, ctx: &egui::Context) {
        let lang = self.language;
        let Some(draft) = &mut self.label_draft else {
            return;
        };
        let opening = std::mem::take(&mut draft.focus);
        let relabeling = matches!(draft.target, LabelTarget::Existing(_));
        let mut selected = None;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("shape-label-picker")).show(ctx, |ui| {
            ui.set_width(320.);
            ui.heading(if relabeling {
                lang.text("Change label", "Đổi nhãn")
            } else {
                lang.text("Choose label", "Chọn nhãn")
            });
            let field = ui.add(
                egui::TextEdit::singleline(&mut draft.input)
                    .hint_text(lang.text("Find or type a label", "Tìm hoặc nhập tên nhãn"))
                    .desired_width(f32::INFINITY),
            );
            if opening {
                field.request_focus();
            }
            let query = draft.input.trim().to_lowercase();
            let recent = &self.label;
            let matches = |label: &str| !label.is_empty() && label.to_lowercase().contains(&query);
            egui::ScrollArea::vertical()
                .max_height(160.)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for label in std::iter::once(recent)
                        .chain(self.classes.labels().filter(|l| *l != recent))
                        .filter(|label| matches(label))
                    {
                        if ui
                            .horizontal(|ui| {
                                color_swatch(ui, self.classes.color(label));
                                ui.add(egui::Button::new(label).truncate().frame(false))
                                    .on_hover_text(label)
                            })
                            .inner
                            .clicked()
                        {
                            selected = Some(label.clone());
                        }
                    }
                });
            if !draft.error.is_empty() {
                ui.colored_label(Color32::LIGHT_RED, &draft.error);
            }
            ui.horizontal(|ui| {
                if ui.button(lang.text("Cancel", "Hủy")).clicked() {
                    cancel = true;
                }
                let can_add = !draft.input.trim().is_empty();
                if ui
                    .add_enabled(
                        can_add,
                        egui::Button::new(if relabeling {
                            lang.text("Apply label", "Áp dụng nhãn")
                        } else {
                            lang.text("Add label", "Thêm nhãn")
                        }),
                    )
                    .clicked()
                    || (!opening && can_add && ctx.input(|i| i.key_pressed(Key::Enter)))
                {
                    selected = Some(draft.input.clone());
                }
            });
        });
        if let Some(label) = selected {
            self.confirm_label(&label);
        } else if cancel || (!opening && modal.should_close()) {
            self.label_draft = None;
        }
    }

    fn delete_selected(&mut self) {
        if !self.draft_points.is_empty() || self.label_draft.is_some() {
            return;
        }
        let Some(frame) = &mut self.frame else { return };
        if let Some(index) = frame.selected.take() {
            self.click_cycle = ClickCycle::default();
            Self::checkpoint(frame);
            shapes_mut(&mut frame.snapshot.doc)
                .expect("validated shapes")
                .remove(index);
            frame.hidden = frame
                .hidden
                .iter()
                .filter_map(|&id| match id.cmp(&index) {
                    std::cmp::Ordering::Less => Some(id),
                    std::cmp::Ordering::Greater => Some(id - 1),
                    std::cmp::Ordering::Equal => None,
                })
                .collect();
            self.save();
        }
    }

    fn undo(&mut self) {
        if !self.draft_points.is_empty() {
            self.draft_points.pop();
            return;
        }
        if self.label_draft.is_some() {
            return;
        }
        if let Some(frame) = &mut self.frame
            && let Some(items) = frame.undo.pop()
        {
            let before = std::mem::replace(
                shapes_mut(&mut frame.snapshot.doc).expect("validated shapes"),
                items,
            );
            frame.redo.push(before);
            frame.selected = None;
            frame.hidden.clear();
            frame.dirty = true;
            self.click_cycle = ClickCycle::default();
            self.save();
        }
    }

    fn redo(&mut self) {
        if !self.draft_points.is_empty() || self.label_draft.is_some() {
            return;
        }
        if let Some(frame) = &mut self.frame
            && let Some(items) = frame.redo.pop()
        {
            let before = std::mem::replace(
                shapes_mut(&mut frame.snapshot.doc).expect("validated shapes"),
                items,
            );
            frame.undo.push(before);
            frame.selected = None;
            frame.hidden.clear();
            frame.dirty = true;
            self.click_cycle = ClickCycle::default();
            self.save();
        }
    }

    fn finish_drag(&mut self, drag: Drag, position: Point) {
        let Some(frame) = &mut self.frame else { return };
        let Some(item) = shapes(&frame.snapshot.doc)
            .ok()
            .and_then(|items| items.get(drag.index))
        else {
            return;
        };
        let Some(kind) = item["shape_type"].as_str() else {
            return;
        };
        let points = drag.positions(position, frame.snapshot.size);
        if points == drag.points {
            return;
        }
        match shape("validated", kind, &points, frame.snapshot.size) {
            Ok(updated) => {
                Self::checkpoint(frame);
                shapes_mut(&mut frame.snapshot.doc).expect("validated shapes")[drag.index]["points"] =
                    updated["points"].clone();
                self.save();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn reload(&mut self, discard: bool) {
        let (Some(workspace), Some(frame)) = (&self.workspace, &mut self.frame) else {
            return;
        };
        match workspace.inspect(&frame.name) {
            Ok(snapshot) => {
                if discard || snapshot.revision != frame.snapshot.revision {
                    self.workspace_cache_dirty |= remember_classes(&mut self.classes, &snapshot.doc);
                    frame.snapshot = snapshot;
                    frame.selected = None;
                    frame.undo.clear();
                    frame.redo.clear();
                    frame.hidden.clear();
                    frame.dirty = false;
                    self.draft_points.clear();
                    self.start = None;
                    self.annotation_drag = None;
                    self.click_cycle = ClickCycle::default();
                    self.label_draft = None;
                    self.status = self.language.text("Annotations reloaded", "Đã nạp lại nhãn").into();
                } else {
                    frame.snapshot.stamp = snapshot.stamp;
                }
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (area, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0., Color32::from_rgb(26, 27, 28));
        let Some(frame) = &mut self.frame else {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                if self.scan.is_some() {
                    self.language.text("Opening workspace...", "Đang mở bộ ảnh…")
                } else {
                    self.language.text("No image selected", "Chưa chọn ảnh")
                },
                egui::FontId::proportional(18.),
                Color32::GRAY,
            );
            return;
        };
        let [w, h] = frame.snapshot.size;
        let natural = vec2(w as f32, h as f32);
        let fit = ((area.width() - 24.) / natural.x)
            .min((area.height() - 24.) / natural.y)
            .max(0.001);
        let interactive = self.label_draft.is_none()
            && self.file_dialog.is_none()
            && !self.discard
            && self.scan.is_none()
            && self.format_job.is_none();
        if interactive && self.annotation_drag.is_none() && response.hovered() {
            let factor = ui.input(|i| {
                // Fit cancels the remainder of a smoothed wheel gesture.
                if self.zoom_needs_input {
                    self.zoom_needs_input = !i.events.iter().any(|event| match event {
                        egui::Event::MouseWheel { modifiers, .. } => {
                            modifiers.ctrl || modifiers.command || modifiers.mac_cmd
                        }
                        egui::Event::Zoom(_) => true,
                        _ => false,
                    });
                }
                if self.zoom_needs_input {
                    1.
                } else {
                    i.zoom_delta()
                }
            });
            if factor != 1.
                && let Some(pointer) = response.hover_pos()
            {
                let next = (self.zoom * factor).clamp(0.25, 24.);
                let origin = pointer - area.center();
                self.pan = origin - (origin - self.pan) * (next / self.zoom);
                self.zoom = next;
            }
        }
        let scale = fit * self.zoom;
        let mut image_rect = Rect::from_center_size(area.center() + self.pan, natural * scale);
        let edit_mode = self.mode == Mode::Select;
        let items = shapes(&frame.snapshot.doc)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let visible = |i| self.show_shapes && !frame.hidden.contains(&i);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let primary_started =
            interactive && edit_mode && response.drag_started_by(egui::PointerButton::Primary);
        if primary_started
            || (interactive
                && edit_mode
                && response.hovered()
                && !ui.input(|i| i.pointer.any_down()))
        {
            let pointer = if primary_started {
                ui.input(|i| i.pointer.press_origin())
            } else {
                pointer
            };
            if let Some(pointer) = pointer {
                let screen = |p: Point| image_rect.min + vec2(p[0] as f32, p[1] as f32) * scale;
                let selected_handle = frame.selected.filter(|&idx| visible(idx)).and_then(|idx| {
                    let item = items.get(idx)?;
                    let points: Vec<_> = read_points(item).into_iter().map(screen).collect();
                    let edit = match (item["shape_type"].as_str()?, points.as_slice()) {
                        ("rectangle", [a, b]) => handles_at(Rect::from_two_pos(*a, *b), pointer),
                        ("polygon" | "line" | "linestrip" | "circle", points) => points
                            .iter()
                            .enumerate()
                            .map(|(i, p)| (i, p.distance_sq(pointer)))
                            .filter(|(_, d)| *d <= 8. * 8.)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| {
                                if i == 0 && item["shape_type"] == "circle" {
                                    Edit::MoveCircle
                                } else {
                                    Edit::Vertex(i)
                                }
                            }),
                        _ => None,
                    }?;
                    Some((idx, edit))
                });
                let intent = selected_handle.or_else(|| {
                    let mut hits = hit_order(items, pointer, screen);
                    hits.retain(|&idx| visible(idx));
                    frame
                        .selected
                        .filter(|idx| hits.contains(idx))
                        .or_else(|| hits.first().copied())
                        .filter(|&idx| {
                            items[idx]["shape_type"]
                                .as_str()
                                .is_some_and(|kind| rustlabel::SHAPE_TYPES.contains(&kind))
                        })
                        .map(|idx| {
                            if items[idx]["shape_type"] == "circle" {
                                (idx, Edit::MoveCircle)
                            } else {
                                (idx, Edit::Move)
                            }
                        })
                });
                if let Some((index, edit)) = intent {
                    ui.ctx().set_cursor_icon(edit.cursor(primary_started));
                    if primary_started {
                        let origin = (pointer - image_rect.min) / scale;
                        self.annotation_drag = Some(Drag {
                            index,
                            edit,
                            origin: [f64::from(origin.x), f64::from(origin.y)],
                            points: read_points(&items[index]),
                        });
                        self.click_cycle = ClickCycle::default();
                    }
                }
            }
        }
        if interactive
            && self.annotation_drag.is_none()
            && (response.dragged_by(egui::PointerButton::Middle)
                || (edit_mode && response.dragged_by(egui::PointerButton::Primary)))
        {
            let delta = ui.input(|i| i.pointer.delta());
            self.pan += delta;
            image_rect = image_rect.translate(delta);
        }
        let to_image = |p: Pos2| -> Point {
            let delta = (p - image_rect.min) / scale;
            [
                delta.x.clamp(0., natural.x) as f64,
                delta.y.clamp(0., natural.y) as f64,
            ]
        };
        let to_screen = |p: Point| image_rect.min + vec2(p[0] as f32, p[1] as f32) * scale;
        let pointer_in_image = pointer.map(|p| {
            let delta = (p - image_rect.min) / scale;
            [f64::from(delta.x), f64::from(delta.y)]
        });
        let click = if interactive && response.clicked_by(egui::PointerButton::Primary) {
            response.interact_pointer_pos()
        } else {
            None
        };
        if matches!(self.mode, Mode::Select | Mode::Box)
            && let Some(position) = click
        {
            let tag_hit = self
                .tag_rects
                .iter()
                .rev()
                .find(|(rect, idx)| rect.contains(position) && visible(*idx))
                .map(|(_, idx)| *idx);
            let selected = if let Some(idx) = tag_hit {
                Some(idx)
            } else {
                let mut hits = hit_order(items, position, to_screen);
                hits.retain(|&idx| visible(idx));
                self.click_cycle.choose(position, hits)
            };
            if frame.selected != selected {
                frame.selected = selected;
                if let Some(label) = selected.and_then(|i| items[i]["label"].as_str()) {
                    self.label = label.into();
                }
                // The sidebar was drawn before the canvas handled the click.
                ui.ctx().request_repaint();
            }
        }
        let active_index = self
            .annotation_drag
            .as_ref()
            .map(|d| d.index)
            .or(frame.selected);
        painter.image(
            frame.texture.id(),
            image_rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
            Color32::WHITE,
        );

        let mut tags = Vec::new();
        {
            for idx in (0..items.len())
                .filter(|&idx| Some(idx) != active_index)
                .chain(active_index)
                .filter(|&idx| visible(idx))
            {
                let Some(item) = items.get(idx) else { continue };
                let points = match (&self.annotation_drag, pointer_in_image) {
                    (Some(drag), Some(pointer)) if drag.index == idx => {
                        ui.ctx().set_cursor_icon(drag.edit.cursor(true));
                        drag.positions(pointer, frame.snapshot.size)
                    }
                    _ => read_points(item),
                };
                let points: Vec<_> = points.into_iter().map(to_screen).collect();
                let selected = active_index == Some(idx);
                let label = item["label"].as_str().unwrap_or("?");
                let color = self.classes.color(label);
                let kind = item["shape_type"].as_str().unwrap_or("polygon");
                if points.is_empty() {
                    continue;
                }
                let mut bounds = Rect::from_points(&points);
                let stroke = Stroke::new(if selected { 2_f32 } else { 1.5_f32 }, color);
                paint_shape(&painter, kind, &points, stroke);
                if let ("circle", [a, b]) = (kind, points.as_slice()) {
                    bounds = Rect::from_center_size(*a, Vec2::splat(a.distance(*b) * 2.));
                }
                if selected && edit_mode {
                    if kind == "rectangle" && points.len() == 2 {
                        for (x, y) in HANDLES {
                            let center = handle(bounds, x, y);
                            painter.rect_filled(
                                Rect::from_center_size(center, Vec2::splat(8.)),
                                1.,
                                Color32::from_rgb(35, 34, 32),
                            );
                            painter.rect_filled(
                                Rect::from_center_size(center, Vec2::splat(5.)),
                                1.,
                                color,
                            );
                        }
                    } else if matches!(kind, "polygon" | "circle" | "line" | "linestrip") {
                        for point in points {
                            painter.circle_filled(point, 3.5, color);
                        }
                    }
                }
                if self.show_names && bounds.intersects(area) {
                    tags.push((idx, bounds, label, color));
                }
            }
        }
        let mut occupied = Vec::with_capacity(tags.len());
        self.tag_rects.clear();
        // The selected shape is drawn last, so give its tag the first choice of space.
        for (idx, bounds, label, color) in tags.into_iter().rev() {
            let rgb = egui::Rgba::from(color);
            let luminance = 0.2126 * rgb.r() + 0.7152 * rgb.g() + 0.0722 * rgb.b();
            let ink = if luminance > 0.179 {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            let mut job = egui::text::LayoutJob::simple_singleline(
                label.into(),
                egui::FontId::proportional(11.),
                ink,
            );
            job.wrap.max_width = (area.width() - 8.).clamp(1., 260.);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = painter.layout_job(job);
            if let Some(tag) = tag_bounds(bounds, area, galley.size() + vec2(8., 2.), &occupied) {
                painter.rect_filled(tag, 2., color);
                painter.galley(tag.left_top() + vec2(4., 1.), galley, ink);
                occupied.push(tag);
                self.tag_rects.push((tag, idx));
            }
        }

        if let Some(LabelDraft { target: LabelTarget::New { kind, points }, .. }) = &self.label_draft {
            let points: Vec<_> = points.iter().copied().map(to_screen).collect();
            paint_shape(&painter, kind, &points, Stroke::new(2_f32, SELECTED));
            return;
        }
        if !interactive {
            return;
        }
        if edit_mode
            && response.drag_stopped_by(egui::PointerButton::Primary)
            && let Some(drag) = self.annotation_drag.take()
        {
            let index = drag.index;
            let frame = self.frame.as_mut().expect("loaded");
            frame.selected = Some(index);
            if let Some(position) = pointer_in_image {
                self.finish_drag(drag, position);
            }
            return;
        }
        if let Some(kind) = self.mode.drag_kind() {
            if response.drag_started_by(egui::PointerButton::Primary) {
                self.start = ui
                    .input(|i| i.pointer.press_origin())
                    .filter(|p| image_rect.contains(*p));
            }
            if let (Some(start), Some(end)) = (self.start, ui.input(|i| i.pointer.interact_pos())) {
                let a = to_image(start);
                let mut b = to_image(end);
                if kind == "circle" {
                    let dx = b[0] - a[0];
                    let dy = b[1] - a[1];
                    let radius = dx.hypot(dy);
                    let limit = a[0]
                        .min(a[1])
                        .min(f64::from(w) - a[0])
                        .min(f64::from(h) - a[1]);
                    if radius > limit && radius > 0. {
                        b = [a[0] + dx * limit / radius, a[1] + dy * limit / radius];
                    }
                }
                let drawn_end = to_screen(b);
                paint_shape(
                    &painter,
                    kind,
                    &[start, drawn_end],
                    Stroke::new(2_f32, SELECTED),
                );
                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    self.start = None;
                    if start.distance(drawn_end) >= 3.
                        && (kind != "rectangle"
                            || ((start.x - drawn_end.x).abs() >= 3.
                                && (start.y - drawn_end.y).abs() >= 3.))
                    {
                        self.queue_label(kind, vec![a, b]);
                    }
                }
            }
        }
        if !self.draft_points.is_empty() {
            let mut draft: Vec<_> = self.draft_points.iter().copied().map(to_screen).collect();
            for &p in &draft {
                painter.circle_filled(p, 3., SELECTED);
            }
            if let Some(p) = response.hover_pos().filter(|p| image_rect.contains(*p)) {
                draft.push(p);
            }
            if draft.len() > 1 {
                painter.add(egui::Shape::line(draft, Stroke::new(1.5_f32, SELECTED)));
            }
        }
        if let Some(position) = click.filter(|p| image_rect.contains(*p)) {
            match self.mode {
                Mode::Point => {
                    self.queue_label("point", vec![to_image(position)]);
                }
                Mode::Polygon | Mode::LineStrip => {
                    if self.mode == Mode::Polygon
                        && self.draft_points.len() >= 3
                        && position.distance(to_screen(self.draft_points[0])) < 9.
                    {
                        self.finish_path();
                    } else {
                        self.draft_points.push(to_image(position));
                    }
                }
                _ => {}
            }
        }
    }

}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.file_dialog.is_none() {
            self.handle_file_drop(ctx);
        }
        if let Some(job) = &self.format_job {
            match job.try_recv() {
                Ok(Ok((path, true))) => {
                    self.format_job = None;
                    self.open(ctx, path);
                }
                Ok(Ok((path, false))) => {
                    self.format_job = None;
                    self.status = self.language.text(
                        format!("Exported to {}", path.display()),
                        format!("Đã xuất vào {}", path.display()),
                    );
                }
                Ok(Err(error)) => {
                    self.format_job = None;
                    self.status = format!("{error:#}");
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.format_job = None;
                    self.status = self.language.text("Dataset conversion stopped.", "Đã dừng chuyển đổi bộ dữ liệu.").into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(scan) = &self.scan {
            match scan.try_recv() {
                Ok(Ok(opened)) => {
                    self.finish_workspace_scan(ctx, opened);
                }
                Ok(Err(e)) => {
                    self.status = format!("{e:#}");
                    self.scan = None;
                    self.refreshing_workspace = false;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.refreshing_workspace = false;
                    self.status = self.language.text("Scan stopped.", "Đã dừng quét.").into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        self.poll_class_scan();
        if self.format_job.is_some() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if self.pending() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.discard = true;
        } else if ctx.input(|i| i.viewport().close_requested()) {
            self.save_session_cache();
        }
        if self.frame.is_some() {
            if self.refresh_at.elapsed() >= Duration::from_secs(2) {
                self.refresh_at = Instant::now();
                if !self.pending()
                    && self.annotation_drag.is_none()
                    && self.start.is_none()
                    && let (Some(workspace), Some(frame)) = (&self.workspace, &self.frame)
                {
                    if workspace
                        .stamp(&frame.name)
                        .is_ok_and(|stamp| stamp != frame.snapshot.stamp)
                    {
                        self.reload(false);
                    }
                }
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        self.fit_shortcut(ctx);
        if self.file_dialog.is_none()
            && self.label_draft.is_none()
            && self.annotation_drag.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::S))
        {
            self.save();
        }
        self.file_shortcuts(ctx);
        self.navigation_shortcut(ctx);
        self.relabel_shortcut(ctx);
        self.tool_shortcut(ctx);
        if !ctx.wants_keyboard_input()
            && !self.discard
            && self.label_draft.is_none()
            && self.file_dialog.is_none()
            && self.format_job.is_none()
        {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.draft_points.clear();
                self.start = None;
                self.annotation_drag = None;
            }
            if ctx.input(|i| i.key_pressed(Key::Enter)) && !self.draft_points.is_empty() {
                self.finish_path();
            }
            if self.annotation_drag.is_none()
                && ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
            {
                self.delete_selected();
            }
            if self.annotation_drag.is_none() {
                if ctx.input_mut(|i| {
                    i.consume_key(
                        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
                        Key::Z,
                    ) || i.consume_key(egui::Modifiers::COMMAND, Key::Y)
                }) {
                    self.redo();
                } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Z)) {
                    self.undo();
                }
            }
        }
        let mut save_cache = false;
        let lang = self.language;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if let Some(mark) = &self.brand_mark {
                    ui.image((mark.id(), vec2(22., 22.)));
                }
                ui.heading("RustLabel");
                ui.separator();
                ui.add_enabled_ui(self.scan.is_none() && self.format_job.is_none(), |ui| {
                    enum OpenAction {
                        PickFiles,
                        PickFolder,
                        OpenRecent(PathBuf),
                        ClearRecents,
                    }
                    let mut action = None;
                    ui.menu_button(lang.text("Open", "Mở"), |ui| {
                        if ui.button(lang.text("Files...", "Chọn ảnh…")).clicked() {
                            action = Some(OpenAction::PickFiles);
                            ui.close_menu();
                        }
                        if ui.button(lang.text("Folder...", "Chọn thư mục…")).clicked() {
                            action = Some(OpenAction::PickFolder);
                            ui.close_menu();
                        }
                        if !self.recent_workspaces.is_empty() {
                            ui.separator();
                            ui.menu_button(lang.text("Recent Workspaces", "Thư mục gần đây"), |ui| {
                                for path in &self.recent_workspaces {
                                    let display = path.display().to_string();
                                    if ui.button(&display).on_hover_text(&display).clicked() {
                                        action = Some(OpenAction::OpenRecent(path.clone()));
                                        ui.close_menu();
                                    }
                                }
                                ui.separator();
                                if ui.button(lang.text("Clear Recent Workspaces", "Xóa lịch sử thư mục")).clicked() {
                                    action = Some(OpenAction::ClearRecents);
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                    match action {
                        Some(OpenAction::PickFiles) => {
                            if let Some(paths) = rfd::FileDialog::new()
                                .add_filter(lang.text("Images", "Ảnh"), &IMAGE_EXTENSIONS)
                                .pick_files()
                            {
                                self.open_source(ctx, WorkspaceSource::Files(paths), false);
                            }
                        }
                        Some(OpenAction::PickFolder) => {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                self.open(ctx, path);
                            }
                        }
                        Some(OpenAction::OpenRecent(path)) => {
                            self.open(ctx, path);
                        }
                        Some(OpenAction::ClearRecents) => {
                            self.recent_workspaces.clear();
                            save_cache = true;
                        }
                        None => {}
                    }
                });
                self.format_menu(ui, ctx);
                ui.menu_button(lang.text("View", "Hiển thị"), |ui| {
                    if ui
                        .checkbox(&mut self.show_shapes, lang.text("Show annotations", "Hiện vùng nhãn"))
                        .changed()
                    {
                        self.click_cycle = ClickCycle::default();
                        save_cache = true;
                    }
                    if ui.checkbox(&mut self.show_names, lang.text("Show label names", "Hiện tên nhãn")).changed() {
                        save_cache = true;
                    }
                });
                ui.menu_button(lang.text("Panels", "Bố cục"), |ui| {
                    if ui.button(lang.text("Reset layout", "Đặt lại bố cục")).clicked() {
                        self.sidebar_dock = Some(default_dock());
                        save_cache = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button(lang.text("Language", "Ngôn ngữ"), |ui| {
                    for (language, name) in [
                        (Language::Vietnamese, "Tiếng Việt"),
                        (Language::English, "English"),
                    ] {
                        if ui.selectable_value(&mut self.language, language, name).clicked() {
                            save_cache = true;
                            ui.close_menu();
                        }
                    }
                });
                if icon_button(ui, Icon::Previous, lang.text("Previous image", "Ảnh trước")).clicked() {
                    self.navigate(ctx, false);
                }
                if icon_button(ui, Icon::Next, lang.text("Next image", "Ảnh sau")).clicked() {
                    self.navigate(ctx, true);
                }
                ui.separator();
                if ui.button(lang.text("Fit", "Vừa khung")).on_hover_text("Ctrl+F / Cmd+F").clicked() {
                    self.fit_view();
                }
                if ui
                    .add_enabled(
                        self.frame.as_ref().is_some_and(|f| f.dirty),
                        egui::Button::new(lang.text("Save", "Lưu")),
                    )
                    .clicked()
                {
                    self.save();
                }
            });
        });
        if save_cache {
            self.save_session_cache();
        }
        let lang = self.language;
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(f) = &self.frame {
                    let [w, h] = f.snapshot.size;
                    ui.add(
                        egui::Label::new(format!(
                            "{}  ·  {w} × {h}  ·  {}%",
                            f.name,
                            (self.zoom * 100.).round()
                        ))
                        .truncate(),
                    );
                    if f.dirty {
                        ui.colored_label(SELECTED, lang.text("Unsaved", "Chưa lưu"));
                    }
                }
                if !self.status.is_empty() {
                    ui.add(egui::Label::new(&self.status).truncate())
                        .on_hover_text(&self.status);
                }
            });
        });
        egui::SidePanel::left("drawing-tools")
            .resizable(false)
            .exact_width(48.)
            .show(ctx, |ui| {
                ui.add_space(3.);
                ui.vertical_centered(|ui| {
                    for (mode, en, vi, _) in Mode::TOOLS {
                        if icon_button(ui, Icon::Tool(mode, self.mode == mode), lang.text(en, vi)).clicked() {
                            self.select_tool(mode);
                        }
                    }
                });
            });
        let mut dock = self.sidebar_dock.take().unwrap_or_else(default_dock);
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut viewer = SidebarViewer(|ui: &mut egui::Ui, tab| {
                if tab == SidebarTab::Files {
                    self.files_ui(ui);
                    return;
                }
                if tab == SidebarTab::Image {
                    self.canvas(ui);
                    return;
                }
                if tab == SidebarTab::Help {
                    help_ui(ui, lang);
                    return;
                }
                if tab == SidebarTab::Classes {
                    ui.weak(lang.text(format!("{} classes", self.classes.len()), format!("{} lớp", self.classes.len())));
                    if let Some(scan) = &self.class_scan && !scan.warm {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak(lang.text(
                                format!("Scanning annotations ({}/{})", scan.scanned, scan.total),
                                format!("Đang quét nhãn ({}/{})", scan.scanned, scan.total),
                            ));
                        });
                    }
                    if self.class_scan_errors > 0 {
                        ui.weak(lang.text(
                            format!("{} annotations skipped", self.class_scan_errors),
                            format!("Đã bỏ qua {} tệp nhãn", self.class_scan_errors),
                        ));
                    }
                    if self.class_cache_error {
                        ui.weak(lang.text("Unable to save the class cache", "Không lưu được bộ nhớ đệm của lớp"));
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("class-list")
                        .show(ui, |ui| {
                            for label in self.classes.labels() {
                                ui.horizontal(|ui| {
                                    color_swatch(ui, self.classes.color(label));
                                    ui.add(egui::Label::new(label).truncate())
                                        .on_hover_text(label);
                                });
                            }
                        });
                    return;
                }
                if self.format_job.is_some() {
                    ui.disable();
                }
                ui.horizontal_wrapped(|ui| {
                    let selected = self.frame.as_ref().is_some_and(|f| f.selected.is_some());
                    if ui
                        .add_enabled(selected, egui::Button::new(lang.text("Delete", "Xóa")))
                        .clicked()
                    {
                        self.delete_selected();
                    }
                    if icon_button(ui, Icon::Undo, lang.text("Undo (Ctrl+Z)", "Hoàn tác (Ctrl+Z)")).clicked() {
                        self.undo();
                    }
                    if icon_button(ui, Icon::Redo, lang.text("Redo (Ctrl+Shift+Z)", "Làm lại (Ctrl+Shift+Z)")).clicked() {
                        self.redo();
                    }
                    if icon_button(ui, Icon::Reload, lang.text("Reload annotations", "Nạp lại nhãn")).clicked() {
                        if self.pending() {
                            self.discard = true;
                        } else {
                            self.reload(true);
                        }
                    }
                });
                if !self.draft_points.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button(lang.text("Finish shape", "Hoàn tất hình")).clicked() {
                            self.finish_path();
                        }
                        if ui.button(lang.text("Cancel", "Hủy")).clicked() {
                            self.draft_points.clear();
                        }
                    });
                }
                ui.separator();
                self.annotation_rows(ui);
            }, None, lang);
            DockArea::new(&mut dock)
                .id(egui::Id::new("annotation-dock"))
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut viewer);
            if let Some(id) = viewer.1 {
                self.file_focus = Some(id);
            }
        });
        self.sidebar_dock = Some(dock);
        if self.discard {
            egui::Window::new(lang.text("Discard unsaved annotations?", "Bỏ các chỉnh sửa chưa lưu?"))
                .id(egui::Id::new("discard-annotations"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(lang.text("Keep editing", "Tiếp tục chỉnh sửa")).clicked() {
                            self.discard = false;
                            self.closing = false;
                        }
                        if ui.button(lang.text("Discard", "Bỏ chỉnh sửa")).clicked() {
                            self.discard = false;
                            if self.closing {
                                self.closing = false;
                                self.draft_points.clear();
                                self.label_draft = None;
                                if let Some(frame) = &mut self.frame {
                                    frame.dirty = false;
                                }
                                self.save_session_cache();
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            } else {
                                self.reload(true);
                            }
                        }
                    });
                });
        } else if self.file_dialog.is_some() {
            self.show_file_dialog(ctx);
        } else {
            self.label_picker(ctx);
        }
        self.save_workspace_cache();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_partial_class_scan();
    }
}

#[cfg(test)]
mod label_picker_tests {
    use super::*;
    use serde_json::json;

    fn editor() -> App {
        let ctx = egui::Context::default();
        App {
            language: Language::English,
            frame: Some(FrameImage {
                name: "test.png".into(),
                texture: ctx.load_texture(
                    "test",
                    egui::ColorImage::new([1, 1], Color32::BLACK),
                    egui::TextureOptions::NEAREST,
                ),
                snapshot: Snapshot {
                    doc: json!({"shapes":[]}),
                    revision: "missing".into(),
                    size: [400, 300],
                    stamp: None,
                },
                dirty: false,
                undo: Vec::new(),
                redo: Vec::new(),
                hidden: BTreeSet::new(),
                selected: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn rectangle_stays_draft_until_valid_label_is_confirmed() {
        let mut app = editor();
        let points = vec![[55., 60.], [20., 25.]];
        assert!(app.queue_label("rectangle", points));
        assert!(app.pending());
        assert!(!app.confirm_label(" "));
        assert!(
            shapes(&app.frame.as_ref().unwrap().snapshot.doc)
                .unwrap()
                .is_empty()
        );
        assert!(app.confirm_label(" car "));
        assert!(app.label_draft.is_none());
        let frame = app.frame.as_ref().unwrap();
        assert!(frame.dirty);
        assert_eq!(shapes(&frame.snapshot.doc).unwrap()[0]["label"], "car");
        assert_eq!(
            shapes(&frame.snapshot.doc).unwrap()[0]["points"],
            json!([[20., 25.], [55., 60.]])
        );
        assert!(app.classes.labels().any(|label| label == "car"));
    }

    #[test]
    fn cancel_does_not_add_shape_or_dirty_the_image() {
        let mut app = editor();
        assert!(app.queue_label("point", vec![[2., 3.]]));
        app.label_draft = None;
        assert!(!app.pending());
        assert!(
            shapes(&app.frame.as_ref().unwrap().snapshot.doc)
                .unwrap()
                .is_empty()
        );
    }

    fn editor_on_disk() -> (App, tempfile::TempDir) {
        let mut app = editor();
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(400, 300)
            .save(dir.path().join("test.png"))
            .unwrap();
        let workspace = Workspace::open(dir.path()).unwrap();
        app.frame.as_mut().unwrap().snapshot = workspace.inspect("test.png").unwrap();
        app.workspace = Some(workspace);
        (app, dir)
    }

    #[test]
    fn annotation_rows_support_relabel_copy_and_delete() {
        let (mut app, _dir) = editor_on_disk();
        for (label, point) in [("car", [10., 15.]), ("bus", [70., 75.])] {
            assert!(app.queue_label("point", vec![point]));
            assert!(app.confirm_label(label));
        }
        let ctx = egui::Context::default();
        let render = |app: &mut App, events, modifiers| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(560., 400.))),
                    modifiers,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.relabel_shortcut(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| app.annotation_rows(ui));
                },
            )
        };
        let position = |output: &egui::FullOutput, text| {
            output
                .shapes
                .iter()
                .find_map(|clipped| {
                    if let egui::Shape::Text(shape) = &clipped.shape
                        && shape.galley.text() == text
                    {
                        return Some(shape.pos + shape.galley.size() / 2.);
                    }
                    None
                })
                .unwrap()
        };
        let none = egui::Modifiers::NONE;
        let click = |app: &mut App, pos, button| {
            let mut send = |pressed| {
                render(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            modifiers: none,
                        },
                    ],
                    none,
                )
            };
            let _ = send(true);
            send(false)
        };
        let rows = render(&mut app, Vec::new(), none);
        click(&mut app, position(&rows, "1  car  ·  point"), egui::PointerButton::Primary);
        let ctrl = egui::Modifiers::CTRL;
        let _ = render(&mut app, vec![egui::Event::Key {
            key: Key::E,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: ctrl,
        }], ctrl);
        assert!(matches!(app.label_draft.take().map(|draft| draft.target), Some(LabelTarget::Existing(0))));

        click(&mut app, position(&rows, "2  bus  ·  point"), egui::PointerButton::Secondary);
        assert_eq!(app.frame.as_ref().unwrap().selected, Some(1));
        let _ = render(&mut app, Vec::new(), none);
        let menu = render(&mut app, Vec::new(), none);
        click(&mut app, position(&menu, "Change label…"), egui::PointerButton::Primary);
        assert!(matches!(
            app.label_draft.as_ref().map(|draft| &draft.target),
            Some(LabelTarget::Existing(1))
        ));
        assert!(app.confirm_label("truck"));
        let saved = app.workspace.as_ref().unwrap().inspect("test.png").unwrap();
        assert_eq!(saved.doc["shapes"][0]["label"], "car");
        assert_eq!(saved.doc["shapes"][1]["label"], "truck");
        assert_eq!(saved.doc["shapes"][1]["points"], json!([[70., 75.]]));

        let rows = render(&mut app, Vec::new(), none);
        let truck = position(&rows, "2  truck  ·  point");
        click(&mut app, truck, egui::PointerButton::Secondary);
        let _ = render(&mut app, Vec::new(), none);
        let menu = render(&mut app, Vec::new(), none);
        let copied = click(
            &mut app,
            position(&menu, "Copy label"),
            egui::PointerButton::Primary,
        );
        assert!(copied.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == "truck")
        }));

        click(&mut app, truck, egui::PointerButton::Secondary);
        let _ = render(&mut app, Vec::new(), none);
        let menu = render(&mut app, Vec::new(), none);
        click(&mut app, position(&menu, "Delete"), egui::PointerButton::Primary);
        let disk = app.workspace.clone().unwrap();
        let deleted = disk.inspect("test.png").unwrap();
        assert_eq!(deleted.doc["shapes"], json!([saved.doc["shapes"][0].clone()]));
        app.undo();
        assert_eq!(disk.inspect("test.png").unwrap().doc, saved.doc);
    }

    #[test]
    fn ctrl_e_relabels_the_selected_shape_without_changing_geometry() {
        let (mut app, _dir) = editor_on_disk();
        assert!(app.queue_label("rectangle", vec![[5., 6.], [55., 66.]]));
        assert!(app.confirm_label("car"));
        app.classes.insert("truck");
        let before = app.frame.as_ref().unwrap().snapshot.doc["shapes"][0].clone();
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers::CTRL;
        let screen_rect = Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 600.)));
        let _ = ctx.run(
            egui::RawInput {
                screen_rect,
                time: Some(1.0),
                modifiers,
                events: vec![egui::Event::Key {
                    key: Key::E,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("canvas")));
                app.relabel_shortcut(ctx);
                app.label_picker(ctx);
            },
        );
        assert!(matches!(
            app.label_draft.as_ref().map(|draft| &draft.target),
            Some(LabelTarget::Existing(0))
        ));
        let output = ctx.run(
            egui::RawInput {
                screen_rect,
                time: Some(2.0),
                ..Default::default()
            },
            |ctx| app.label_picker(ctx),
        );
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "truck")
        }));
        assert!(app.confirm_label("truck"));
        let disk = app.workspace.clone().unwrap();
        let mut expected = before.clone();
        expected["label"] = "truck".into();
        assert_eq!(disk.inspect("test.png").unwrap().doc["shapes"], json!([expected]));
        app.undo();
        assert_eq!(disk.inspect("test.png").unwrap().doc["shapes"], json!([before]));
    }

    #[test]
    fn single_canvas_click_paints_selection_and_cycles_overlaps() {
        let ctx = egui::Context::default();
        let mut app = editor();
        let frame = app.frame.as_mut().unwrap();
        frame.snapshot.doc["shapes"] = json!([
            shape(
                "small",
                "rectangle",
                &[[160., 120.], [240., 180.]],
                [400, 300]
            )
            .unwrap(),
            shape(
                "large",
                "rectangle",
                &[[90., 80.], [310., 220.]],
                [400, 300]
            )
            .unwrap(),
        ]);
        remember_classes(&mut app.classes, &frame.snapshot.doc);
        let large_color = app.classes.color("large");
        let small_color = app.classes.color("small");
        let pointer = pos2(400., 300.);
        let mut render = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                },
            )
        };
        let button = |pressed| egui::Event::PointerButton {
            pos: pointer,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render(vec![egui::Event::PointerMoved(pointer)]);
        for color in [large_color, small_color, large_color] {
            render(vec![button(true)]);
            let release = render(vec![button(false)]);
            assert!(release.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(rect) if rect.fill == color && rect.rect.size() == Vec2::splat(5.))
            }), "the clicked shape must show resize handles in the release frame");
        }
        assert!(!app.pending(), "selecting an annotation must not edit it");
    }

    #[test]
    fn drag_releases_a_line_or_circle_into_the_label_picker() {
        for (mode, expected) in [(Mode::Line, "line"), (Mode::Circle, "circle")] {
            let ctx = egui::Context::default();
            let mut app = editor();
            app.mode = mode;
            let start = pos2(393., 294.);
            let end = pos2(485., 294.);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for events in [
                vec![egui::Event::PointerMoved(start)],
                vec![button(start, true)],
                vec![egui::Event::PointerMoved(end)],
                vec![button(end, false)],
            ] {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                    },
                );
            }
            assert_eq!(
                app.label_draft.as_ref().and_then(|draft| match draft.target {
                    LabelTarget::New { kind, .. } => Some(kind),
                    LabelTarget::Existing(_) => None,
                }),
                Some(expected)
            );
            assert!(app.confirm_label("lane"));
            assert_eq!(
                app.frame.as_ref().unwrap().snapshot.doc["shapes"][0]["shape_type"],
                expected
            );
        }
    }

    #[test]
    fn finishing_a_polyline_preserves_open_path_points() {
        let mut app = editor();
        app.mode = Mode::LineStrip;
        app.draft_points = vec![[10., 15.], [30., 25.], [55., 20.]];
        app.finish_path();
        assert!(app.draft_points.is_empty());
        assert!(app.confirm_label("lane"));
        let shape = &app.frame.as_ref().unwrap().snapshot.doc["shapes"][0];
        assert_eq!(shape["shape_type"], "linestrip");
        assert_eq!(shape["points"], json!([[10., 15.], [30., 25.], [55., 20.]]));
    }

    #[test]
    fn a_visible_rectangle_keeps_its_class_outline_without_a_black_halo() {
        let ctx = egui::Context::default();
        let mut app = editor();
        let frame = app.frame.as_mut().unwrap();
        frame.snapshot.doc["shapes"] = json!([shape(
            "car",
            "rectangle",
            &[[160., 120.], [240., 180.]],
            [400, 300]
        )
        .unwrap()]);
        remember_classes(&mut app.classes, &frame.snapshot.doc);
        let color = app.classes.color("car");

        for show_names in [true, false] {
            app.show_names = show_names;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                },
            );
            let rects = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                });
            assert_eq!(rects.clone().any(|rect| rect.fill == color), show_names);
            let outline = rects
                .clone()
                .find(|rect| rect.stroke.color == color)
                .unwrap();
            assert_eq!(
                rects
                    .filter(|rect| rect.rect == outline.rect && rect.stroke.width > 0.)
                    .count(),
                1,
                "the colored outline must not have a second stroke"
            );
        }
    }

    #[test]
    fn a_hidden_annotation_is_not_hit_or_painted() {
        let ctx = egui::Context::default();
        let mut app = editor();
        let frame = app.frame.as_mut().unwrap();
        frame.snapshot.doc["shapes"] = json!([shape(
            "car",
            "rectangle",
            &[[160., 120.], [240., 180.]],
            [400, 300]
        )
        .unwrap()]);
        frame.hidden.insert(0);
        remember_classes(&mut app.classes, &frame.snapshot.doc);
        let color = app.classes.color("car");
        let pointer = pos2(400., 300.);
        let mut overlay_painted = false;
        for events in [
            vec![egui::Event::PointerMoved(pointer)],
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ] {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                },
            );
            overlay_painted |= output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(rect) if rect.stroke.color == color || rect.fill == color)
            });
        }
        assert!(!overlay_painted);
        assert_eq!(app.frame.as_ref().unwrap().selected, None);
        assert!(!app.pending());
    }

    #[test]
    fn completed_changes_and_undo_autosave() {
        let (mut app, dir) = editor_on_disk();
        assert!(app.queue_label("rectangle", vec![[10., 10.], [100., 100.]]));
        assert!(!dir.path().join("test.json").exists());
        assert!(!app.confirm_label(" "));
        assert!(app.confirm_label("car"));
        assert!(!app.pending());
        let disk = app.workspace.clone().unwrap();
        let drag = Drag {
            index: 0,
            edit: Edit::Move,
            origin: [20., 20.],
            points: vec![[10., 10.], [100., 100.]],
        };
        app.finish_drag(drag, [30., 35.]);
        assert_eq!(
            disk.inspect("test.png").unwrap().doc["shapes"][0]["points"],
            json!([[20., 25.], [110., 115.]])
        );
        app.undo();
        assert_eq!(
            disk.inspect("test.png").unwrap().doc["shapes"][0]["points"],
            json!([[10., 10.], [100., 100.]])
        );
        app.redo();
        app.frame.as_mut().unwrap().selected = Some(0);
        assert_eq!(
            disk.inspect("test.png").unwrap().doc["shapes"][0]["label"],
            "car"
        );
        app.delete_selected();
        assert_eq!(disk.inspect("test.png").unwrap().doc["shapes"], json!([]));
        app.undo();
        assert_eq!(
            disk.inspect("test.png").unwrap().doc["shapes"][0]["label"],
            "car"
        );
        assert!(!app.pending());
        app.redo();
        assert_eq!(disk.inspect("test.png").unwrap().doc["shapes"], json!([]));
        app.undo();
        assert!(app.queue_label("point", vec![[4., 6.]]));
        assert!(app.confirm_label("new"));
        app.redo();
        assert_eq!(
            disk.inspect("test.png").unwrap().doc["shapes"][1]["label"],
            "new"
        );
    }

    #[test]
    fn autosave_conflict_keeps_local_changes_and_external_revision() {
        let (mut app, _dir) = editor_on_disk();
        let disk = app.workspace.clone().unwrap();
        let mut external = disk.inspect("test.png").unwrap().doc;
        external["shapes"] = json!([shape("external", "point", &[[6., 7.]], [400, 300]).unwrap()]);
        disk.save("test.png", "missing", &external).unwrap();
        assert!(app.queue_label("point", vec![[3., 4.]]));
        assert!(app.confirm_label("local"));
        assert!(app.pending());
        assert!(app.status.contains("changed on disk"));
        assert_eq!(disk.inspect("test.png").unwrap().doc, external);
        assert_eq!(
            app.frame.as_ref().unwrap().snapshot.doc["shapes"][0]["label"],
            "local"
        );
    }
    #[test]
    fn folder_scan_finds_classes_in_unopened_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        for name in ["root.png", "nested/unopened.png"] {
            image::RgbImage::new(2, 2)
                .save(dir.path().join(name))
                .unwrap();
        }
        std::fs::write(
            dir.path().join("nested/unopened.json"),
            r#"{"imageData":"unused","shapes":[{"label":"unopened_class"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("root.json"), "not-json").unwrap();
        let mut app = App::default();
        let ctx = egui::Context::default();
        app.scan_classes(Workspace::open(dir.path()).unwrap(), &ctx, 0, 0);
        let scan = app.class_scan.as_ref().unwrap();
        let mut classes = BTreeSet::new();
        loop {
            let update = scan.result.recv_timeout(Duration::from_secs(5)).unwrap();
            classes.extend(update.labels);
            if update.finished {
                assert_eq!(update.scanned, 2);
                assert_eq!(update.skipped, 1);
                break;
            }
        }
        assert_eq!(classes, BTreeSet::from(["unopened_class".into()]));
    }

    #[test]
    fn reopened_workspace_publishes_cached_classes_before_validation() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("workspace");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("one.png"), [])?;
        let sidecar = root.join("one.json");
        std::fs::write(&sidecar, r#"{"shapes":[{"label":"cached"}]}"#)?;
        let cache_path = dir.path().join("session.json");
        let ctx = egui::Context::default();
        let mut first = App {
            session_cache_path_override: Some(cache_path.clone()),
            ..Default::default()
        };
        first.scan_classes(Workspace::open(&root)?, &ctx, 0, 0);
        let first_scan = first.class_scan.as_ref().unwrap();
        loop {
            let update = first_scan.result.recv_timeout(Duration::from_secs(5))?;
            if update.finished {
                assert!(!update.cache_error);
                break;
            }
        }
        drop(first);

        std::fs::write(&sidecar, r#"{"shapes":[{"label":"changed_class"}]}"#)?;
        let mut reopened = App {
            session_cache_path_override: Some(cache_path),
            ..Default::default()
        };
        reopened.scan_classes(Workspace::open(&root)?, &ctx, 0, 0);
        let scan = reopened.class_scan.as_ref().unwrap();
        let cached = scan.result.recv_timeout(Duration::from_secs(5))?;
        assert!(cached.preview);
        assert_eq!(cached.labels, BTreeSet::from(["cached".into()]));
        let mut verified = BTreeSet::new();
        loop {
            let update = scan.result.recv_timeout(Duration::from_secs(5))?;
            verified.extend(update.labels);
            if update.finished {
                break;
            }
        }
        assert_eq!(verified, BTreeSet::from(["changed_class".into()]));
        Ok(())
    }

    #[test]
    fn reopening_a_completed_folder_skips_the_scans_until_refresh() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("workspace");
        std::fs::create_dir(&root)?;
        for name in ["active.png", "labeled.png"] {
            image::RgbImage::new(12, 8).save(root.join(name))?;
        }
        let sidecar = root.join("labeled.json");
        std::fs::write(&sidecar, r#"{"shapes":[{"label":"old_class"}]}"#)?;
        let session = dir.path().join("session.json");
        let ctx = egui::Context::default();
        let finish_open = |app: &mut App| -> anyhow::Result<()> {
            let opened = app.scan.take().unwrap().recv_timeout(Duration::from_secs(5))??;
            app.finish_workspace_scan(&ctx, opened);
            Ok(())
        };
        let finish_classes = |app: &mut App| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.class_scan.is_some() {
                assert!(Instant::now() < deadline, "class scan did not finish");
                app.poll_class_scan();
                if app.class_scan.is_some() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            app.save_workspace_cache();
            assert!(!app.class_cache_error);
        };
        let mut first = App {
            session_cache_path_override: Some(session.clone()),
            ..Default::default()
        };
        first.open(&ctx, root.clone());
        finish_open(&mut first)?;
        assert!(first.class_scan.is_some());
        finish_classes(&mut first);
        assert!(first.classes.contains("old_class"));
        drop(first);

        std::fs::write(&sidecar, r#"{"shapes":[{"label":"changed_class"}]}"#)?;
        image::RgbImage::new(12, 8).save(root.join("new.png"))?;
        let mut reopened = App {
            session_cache_path_override: Some(session),
            ..Default::default()
        };
        reopened.load_session_cache();
        assert!(reopened.restore_last_session(&ctx));
        finish_open(&mut reopened)?;
        assert!(reopened.class_scan.is_none());
        assert_eq!(reopened.images, ["active.png", "labeled.png"]);
        assert!(reopened.classes.contains("old_class"));
        assert!(!reopened.classes.contains("changed_class"));

        reopened.refresh_workspace(&ctx);
        finish_open(&mut reopened)?;
        assert!(reopened.class_scan.is_some());
        finish_classes(&mut reopened);
        assert_eq!(reopened.images, ["active.png", "labeled.png", "new.png"]);
        assert!(reopened.classes.contains("changed_class"));
        assert!(!reopened.classes.contains("old_class"));
        Ok(())
    }

    #[test]
    fn closing_saves_partial_scan_and_reopen_skips_the_processed_images() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("workspace");
        std::fs::create_dir(&root)?;
        for name in ["a.png", "b.png"] {
            image::RgbImage::new(12, 8).save(root.join(name))?;
        }
        // Reading this sidecar again would count it as skipped.
        std::fs::write(root.join("a.json"), "invalid after the first scan")?;
        std::fs::write(root.join("b.json"), r#"{"shapes":[{"label":"remaining"}]}"#)?;
        let session = dir.path().join("session.json");
        let workspace = Workspace::open(&root)?;
        let mut first = App {
            images: workspace.images()?.to_vec(),
            workspace: Some(workspace.clone()),
            session_cache_path_override: Some(session.clone()),
            workspace_cache_ready: true,
            workspace_cache_next_image: Some(0),
            ..Default::default()
        };
        first.save_session_cache();
        let (tx, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker_cancel.load(Ordering::Relaxed) {
                assert!(Instant::now() < deadline, "shutdown did not stop the class scan");
                std::thread::sleep(Duration::from_millis(1));
            }
            tx.send(ClassScanUpdate {
                labels: BTreeSet::from(["already_discovered".into()]),
                scanned: 1,
                finished: true,
                ..Default::default()
            }).unwrap();
        });
        first.class_scan = Some(ClassScan {
            worker: Some(worker),
            workspace,
            cancel,
            result,
            scanned: 0,
            total: 2,
            cached_labels: BTreeSet::new(),
            warm: false,
        });
        eframe::App::on_exit(&mut first, None);

        let ctx = egui::Context::default();
        let mut reopened = App {
            session_cache_path_override: Some(session),
            ..Default::default()
        };
        reopened.load_session_cache();
        assert!(reopened.restore_last_session(&ctx));
        let opened = reopened.scan.take().unwrap().recv_timeout(Duration::from_secs(5))??;
        assert_eq!(opened.cached_next_image, Some(1));
        assert_eq!(opened.classes, BTreeSet::from(["already_discovered".into()]));
        reopened.finish_workspace_scan(&ctx, opened);
        let scan = reopened.class_scan.as_ref().unwrap();
        assert_eq!(scan.scanned, 1);
        let result = scan.result.recv_timeout(Duration::from_secs(5))?;
        assert!(result.finished);
        assert_eq!(result.scanned, 2);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.labels, BTreeSet::from(["remaining".into()]));
        Ok(())
    }

    #[test]
    fn refresh_loads_added_images_and_classes_without_resetting_the_view() -> anyhow::Result<()> {
        let (mut app, dir) = editor_on_disk();
        let root = dir.path();
        app.images = vec!["test.png".into()];
        app.selected_files.insert(0);
        app.classes.insert("removed_class");
        app.zoom = 2.5;
        app.pan = vec2(24., -10.);
        image::RgbImage::new(12, 8).save(root.join("added.png"))?;
        std::fs::write(root.join("added.json"), r#"{"shapes":[{"label":"added_class"}]}"#)?;
        let ctx = egui::Context::default();
        app.refresh_workspace(&ctx);
        let opened = app.scan.take().unwrap().recv_timeout(Duration::from_secs(5))??;
        app.finish_workspace_scan(&ctx, opened);
        assert_eq!(app.images, ["added.png", "test.png"]);
        assert_eq!(app.selected_file_names(), ["test.png"]);
        assert_eq!(app.frame.as_ref().unwrap().name, "test.png");
        assert_eq!((app.zoom, app.pan), (2.5, vec2(24., -10.)));
        assert!(!app.classes.labels().any(|label| label == "removed_class"));
        let scan = app.class_scan.as_ref().unwrap();
        let mut discovered = BTreeSet::new();
        loop {
            let update = scan.result.recv_timeout(Duration::from_secs(5))?;
            discovered.extend(update.labels);
            if update.finished {
                break;
            }
        }
        assert_eq!(discovered, BTreeSet::from(["added_class".into()]));
        Ok(())
    }

    #[test]
    fn dropping_images_indexes_only_those_files_and_saves_beside_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::create_dir(path.join("nested")).unwrap();
        for name in ["a.png", "unselected.png", "nested/b.png"] {
            image::RgbImage::new(16, 16).save(path.join(name)).unwrap();
        }
        let ctx = egui::Context::default();
        let mut app = App::default();
        let raw = egui::RawInput {
            dropped_files: ["nested/b.png", "a.png", "a.png"]
                .into_iter()
                .map(|name| egui::DroppedFile {
                    path: Some(path.join(name)),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.handle_file_drop(ctx));
        let OpenedWorkspace { workspace, images, .. } = app
            .scan
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(images, ["a.png", "nested/b.png"]);
        assert!(workspace.inspect("unselected.png").is_err());
        let mut snapshot = workspace.inspect("nested/b.png").unwrap();
        snapshot.doc["shapes"] = json!([shape("check", "point", &[[4., 4.]], [16, 16]).unwrap()]);
        workspace
            .save("nested/b.png", &snapshot.revision, &snapshot.doc)
            .unwrap();
        assert!(path.join("nested/b.json").exists());
        assert!(!path.join("unselected.json").exists());
    }

    #[test]
    fn ctrl_f_resets_zoom_and_pan() {
        let mut app = App {
            zoom: 3.,
            pan: vec2(30., -20.),
            ..Default::default()
        };
        let keypress = |key| egui::RawInput {
            modifiers: egui::Modifiers::CTRL,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
            ..Default::default()
        };
        let _ = egui::Context::default().run(keypress(Key::T), |ctx| app.fit_shortcut(ctx));
        assert_eq!((app.zoom, app.pan), (3., vec2(30., -20.)));
        let _ = egui::Context::default().run(keypress(Key::F), |ctx| app.fit_shortcut(ctx));
        assert_eq!(app.zoom, 1.);
        assert_eq!(app.pan, Vec2::ZERO);
    }

    #[test]
    fn number_keys_choose_the_first_four_tools() {
        let mut app = App::default();
        for (key, mode) in [
            (Key::Num2, Mode::Box),
            (Key::Num3, Mode::Polygon),
            (Key::Num4, Mode::Circle),
            (Key::Num1, Mode::Select),
        ] {
            let _ = egui::Context::default().run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| app.tool_shortcut(ctx),
            );
            assert_eq!(app.mode, mode);
        }
    }

    #[test]
    fn dragging_the_classes_tab_outside_the_dock_floats_it() {
        #[derive(Default)]
        struct Probe {
            classes: Option<Pos2>,
            annotations: Option<Pos2>,
        }

        impl TabViewer for Probe {
            type Tab = SidebarTab;

            fn title(&mut self, tab: &mut SidebarTab) -> egui::WidgetText {
                format!("{tab:?}").into()
            }

            fn ui(&mut self, _: &mut egui::Ui, _: &mut SidebarTab) {}

            fn on_tab_button(&mut self, tab: &mut SidebarTab, response: &egui::Response) {
                let slot = match tab {
                    SidebarTab::Classes => &mut self.classes,
                    SidebarTab::Annotations => &mut self.annotations,
                    SidebarTab::Image | SidebarTab::Files | SidebarTab::Help | SidebarTab::BatchEdit => return,
                };
                *slot = Some(response.rect.center());
            }
        }

        let ctx = egui::Context::default();
        let mut dock = default_dock();
        let mut frame = |events| {
            let mut viewer = Probe::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        DockArea::new(&mut dock)
                            .show_close_buttons(false)
                            .show_inside(ui, &mut viewer);
                    });
                },
            );
            (viewer.classes, viewer.annotations)
        };
        let (classes, annotations) = frame(vec![]);
        let start = classes.unwrap();
        let dock_target = annotations.unwrap();
        let target = pos2(400., 240.);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(start)]);
        frame(vec![button(start, true)]);
        frame(vec![egui::Event::PointerMoved(start - vec2(15., 0.))]);
        frame(vec![egui::Event::PointerMoved(target)]);
        frame(vec![]);
        frame(vec![button(target, false)]);
        let floated = frame(vec![]).0.unwrap();
        assert!(floated.x < 650., "the Classes tab should be floating");
        frame(vec![egui::Event::PointerMoved(floated)]);
        frame(vec![button(floated, true)]);
        frame(vec![egui::Event::PointerMoved(floated + vec2(0., 12.))]);
        frame(vec![egui::Event::PointerMoved(dock_target)]);
        frame(vec![]);
        frame(vec![button(dock_target, false)]);
        assert!(
            dock.find_main_surface_tab(&SidebarTab::Annotations)
                .is_some()
        );
        assert!(dock.find_main_surface_tab(&SidebarTab::Classes).is_some());
    }

    #[test]
    fn only_modified_wheel_zooms_and_fit_overrides_scroll_momentum() {
        let ctx = egui::Context::default();
        let mut app = editor();
        let pointer = pos2(500., 340.);
        let mut render = |modifiers, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                    modifiers,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.fit_shortcut(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                },
            );
            (app.zoom, app.pan)
        };
        let wheel = |modifiers| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0., 1.),
            modifiers,
        };
        let none = egui::Modifiers::NONE;
        let ctrl = egui::Modifiers::CTRL;
        render(none, vec![egui::Event::PointerMoved(pointer)]);
        assert_eq!(render(none, vec![wheel(none)]), (1., Vec2::ZERO));
        let (zoom, pan) = render(ctrl, vec![wheel(ctrl)]);
        assert!(zoom > 1.);
        assert!((pan - (pointer - pos2(400., 300.)) * (1. - zoom)).length() < 0.01);
        assert_eq!(
            render(
                ctrl,
                vec![egui::Event::Key {
                    key: Key::F,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: ctrl,
                }]
            ),
            (1., Vec2::ZERO)
        );
        assert_eq!(render(ctrl, vec![]), (1., Vec2::ZERO));
        assert!(render(ctrl, vec![wheel(ctrl)]).0 > 1.);
    }

    #[test]
    fn batch_relabel_renames_classes_across_selected_files() {
        let (mut app, dir) = editor_on_disk();
        image::RgbImage::new(400, 300)
            .save(dir.path().join("second.png"))
            .unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let doc1 = json!({
            "imagePath": "test.png",
            "imageWidth": 400,
            "imageHeight": 300,
            "shapes": [shape("cat", "rectangle", &[[10., 10.], [50., 50.]], [400, 300]).unwrap()]
        });
        let doc2 = json!({
            "imagePath": "second.png",
            "imageWidth": 400,
            "imageHeight": 300,
            "shapes": [shape("cat", "rectangle", &[[20., 20.], [60., 60.]], [400, 300]).unwrap()]
        });
        let rev1 = ws.inspect("test.png").unwrap().revision;
        let rev2 = ws.inspect("second.png").unwrap().revision;
        ws.save("test.png", &rev1, &doc1).unwrap();
        ws.save("second.png", &rev2, &doc2).unwrap();

        app.workspace = Some(ws.clone());
        app.images = vec!["test.png".into(), "second.png".into()];
        app.filtered = vec![0, 1];
        app.selected_files = [0, 1].into_iter().collect();

        let names = app.selected_file_names();
        app.apply_batch_relabel(&names, "cat", "feline");

        let updated1 = ws.inspect("test.png").unwrap();
        let updated2 = ws.inspect("second.png").unwrap();
        assert_eq!(updated1.doc["shapes"][0]["label"], "feline");
        assert_eq!(updated2.doc["shapes"][0]["label"], "feline");
    }

    #[test]
    fn session_cache_saves_to_app_and_restores_state() {
        let temp = tempfile::tempdir().unwrap();
        let cache_file = temp.path().join("session.json");

        let mut app = editor();
        app.session_cache_path_override = Some(cache_file.clone());
        app.mode = Mode::Polygon;
        app.label = "car".into();
        app.language = Language::English;
        app.show_shapes = true;
        app.show_names = false;
        let ws_dir = temp.path().join("fake_ws");
        std::fs::create_dir_all(&ws_dir).unwrap();
        app.add_recent_workspace(&ws_dir);
        app.workspace_last_images.insert(clean_path(&ws_dir), "pic2.png".into());

        // Custom dock layout
        let mut dock = default_dock();
        let help = dock.find_tab(&SidebarTab::Help).unwrap();
        dock.remove_tab(help);
        let classes = dock.find_tab(&SidebarTab::Classes).unwrap();
        dock.set_active_tab(classes);
        let (surface, node, _) = dock.find_tab(&SidebarTab::Files).unwrap();
        dock[surface][node].append_tab(SidebarTab::BatchEdit);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(900., 600.));
        for node in dock.main_surface_mut().iter_mut() {
            node.set_rect(rect);
            if let egui_dock::Node::Leaf { viewport, .. } = node {
                *viewport = rect;
            }
        }
        app.sidebar_dock = Some(dock);

        app.save_session_cache();
        assert!(cache_file.is_file(), "cache must be saved to the app cache file");

        let mut restored = App::default();
        restored.session_cache_path_override = Some(cache_file);
        restored.load_session_cache();
        assert_eq!(restored.mode, Mode::Polygon);
        assert_eq!(restored.label, "car");
        assert_eq!(restored.language, Language::English);
        assert!(!restored.show_names);
        assert_eq!(restored.recent_workspaces.len(), 1);
        assert_eq!(
            restored.workspace_last_images.get(&clean_path(&ws_dir)),
            Some(&"pic2.png".to_string())
        );
        assert!(restored.sidebar_dock.is_some(), "dock layout must be restored from cache");
        assert!(restored.sidebar_dock.as_ref().unwrap().find_tab(&SidebarTab::BatchEdit).is_none());
        assert!(restored.sidebar_dock.as_ref().unwrap().find_tab(&SidebarTab::Classes).is_some());
        let dock = restored.sidebar_dock.as_ref().unwrap();
        assert!(dock.find_tab(&SidebarTab::Help).is_some());
        let (surface, node, classes) = dock.find_tab(&SidebarTab::Classes).unwrap();
        assert!(matches!(&dock[surface][node], egui_dock::Node::Leaf { active, .. } if *active == classes));
    }

    #[test]
    fn explicit_file_session_restores_selection_filter_active_image_and_view() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let images = ["a.png", "b.png", "other.png"].map(|name| dir.path().join(name));
        for path in &images {
            image::RgbImage::new(8, 6).save(path)?;
        }
        let ctx = egui::Context::default();
        let cache = dir.path().join("session.json");
        let workspace = Workspace::open_files(&images[..2])?;
        let mut app = App {
            images: workspace.images()?.to_vec(),
            workspace: Some(workspace),
            session_cache_path_override: Some(cache.clone()),
            ..Default::default()
        };
        app.load(&ctx, 1);
        app.filter = "a".into();
        app.filter();
        app.selected_files = [0, 1].into_iter().collect();
        app.zoom = 2.;
        app.pan = vec2(12., -8.);
        app.save_session_cache();

        let mut restored = App {
            session_cache_path_override: Some(cache),
            ..Default::default()
        };
        restored.load_session_cache();
        assert!(restored.restore_last_session(&ctx));
        let opened = restored.scan.take().unwrap()
            .recv_timeout(Duration::from_secs(5))??;
        restored.finish_workspace_scan(&ctx, opened);
        assert!(restored.workspace.as_ref().unwrap().is_file_selection());
        assert_eq!(restored.images, ["a.png", "b.png"]);
        assert_eq!(restored.frame.as_ref().unwrap().name, "b.png");
        assert_eq!(restored.selected_file_names(), ["a.png", "b.png"]);
        assert_eq!(restored.filtered, [0]);
        assert_eq!((restored.zoom, restored.pan), (2., vec2(12., -8.)));
        Ok(())
    }

    #[test]
    fn recent_workspaces_deduplicates_and_caps_at_ten() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::default();
        let mut created = Vec::new();
        for i in 0..12 {
            let p = temp.path().join(format!("ws_{i}"));
            std::fs::create_dir(&p).unwrap();
            created.push(p);
        }
        for p in &created {
            app.add_recent_workspace(p);
        }
        assert_eq!(app.recent_workspaces.len(), 10);
        // Re-adding an existing workspace moves it to front
        app.add_recent_workspace(&created[3]);
        assert_eq!(app.recent_workspaces.len(), 10);
        assert_eq!(app.recent_workspaces[0], clean_path(&created[3]));
    }
}

fn main() -> eframe::Result<()> {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let logo = image::load_from_memory_with_format(LOGO_PNG, image::ImageFormat::Png)
        .expect("bundled RustLabel logo is a valid PNG")
        .into_rgba8();
    let (width, height) = logo.dimensions();
    let icon = egui::IconData {
        width,
        height,
        rgba: logo.as_raw().clone(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280., 800.])
            .with_min_inner_size([860., 560.])
            .with_drag_and_drop(true)
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "RustLabel",
        options,
        Box::new(move |cc| {
            install_ui_fonts(&cc.egui_ctx);
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = Color32::from_rgb(36, 37, 38);
            visuals.window_fill = Color32::from_rgb(42, 43, 44);
            visuals.extreme_bg_color = Color32::from_rgb(29, 30, 31);
            visuals.selection.bg_fill = Color32::from_rgb(54, 79, 73);
            visuals.selection.stroke.color = Color32::from_rgb(226, 229, 227);
            cc.egui_ctx.set_visuals(visuals);
            let mut app = App {
                brand_mark: Some(cc.egui_ctx.load_texture(
                    "RustLabel logo",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &logo,
                    ),
                    egui::TextureOptions::NEAREST,
                )),
                ..Default::default()
            };
            app.load_session_cache();
            match paths.as_slice() {
                [] => {
                    app.restore_last_session(&cc.egui_ctx);
                }
                [path] if path.is_dir() => app.open(&cc.egui_ctx, path.clone()),
                _ => app.open_source(&cc.egui_ctx, WorkspaceSource::Files(paths), false),
            }
            Ok(Box::new(app))
        }),
    )
}
