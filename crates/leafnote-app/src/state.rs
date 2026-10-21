//! Application state: the bridge between the `leafnote-core` engine and the
//! Iced GUI. Everything the widgets need is derived here so the view layer
//! stays declarative.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use leafnote_core::config::Config;
use leafnote_core::import::{self, ImportReport};
use leafnote_core::model::{
    Block, Color, Id, NoteTag, Page, Paragraph, PenKind, Point, Stroke, TreeNode,
};
use leafnote_core::search::{SearchHit, SearchIndex};
use leafnote_core::store::Store;

/// Sidebar selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    Notebook(Id),
    Section(Id),
    Page { section: Id, page: Id },
}

/// A sidebar row (flattened notebook tree).
#[derive(Clone, Debug)]
pub struct TreeRow {
    pub key: String,
    pub depth: usize,
    pub label: String,
    pub icon: &'static str,
    pub selected: bool,
    pub read_only: bool,
    pub sel: Selection,
}

/// Tool active on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Text,
    Pen,
    Pencil,
    Highlighter,
    Eraser,
}

impl Tool {
    pub fn pen_kind(self) -> Option<PenKind> {
        match self {
            Tool::Pen => Some(PenKind::Pen),
            Tool::Pencil => Some(PenKind::Pencil),
            Tool::Highlighter => Some(PenKind::Highlighter),
            Tool::Eraser => Some(PenKind::Eraser),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Text => "Text",
            Tool::Pen => "Pen",
            Tool::Pencil => "Pencil",
            Tool::Highlighter => "Highlight",
            Tool::Eraser => "Erase",
        }
    }
}

/// One undoable action.
#[derive(Clone, Debug)]
pub enum Edit {
    AddBlock(Block),
    RemoveBlock(Id),
    UpdateBlock(Block),
    RenamePage(String),
}

/// Canvas camera (pan & zoom). World units are millimetres.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub pan_x: f32,
    pub pan_y: f32,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self { pan_x: 40.0, pan_y: 30.0, zoom: 3.6 }
    }
}

/// Flattened widget request from the canvas frame back to the app.
#[derive(Clone, Debug)]
pub enum CanvasMsg {
    DragStart(Point),
    DragPoint(Point),
    DragEnd,
    /// Pan the camera by a screen-pixel delta.
    PanBy(f32, f32),
    /// Zoom by `factor` anchored at screen coordinates (x, y).
    ZoomAt(f32, f32, f32),
    /// Outline paragraph edited by index.
    ParaEdited(usize, String),
    /// Clicked a tag icon at outline index → toggle ToDo checked.
    ToggleCheck(usize),
    /// Clicked an attachment chip → open it externally.
    OpenAttachment(Id),
}

/// Where an external file dialog was triggered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickTarget {
    OpenOneNote,
    InsertImage,
    InsertFile,
}

pub struct AppState {
    pub config: Config,
    pub store: Store,
    pub index: SearchIndex,
    pub selection: Option<Selection>,
    pub rows: Vec<TreeRow>,
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub tool: Tool,
    pub color: Color,
    pub width_mm: f32,
    pub view: View,
    pub dirty: bool,
    pub status: String,
    pub last_report: Option<ImportReport>,
    pub pick_target: Option<PickTarget>,
    pub pending_open: Option<PathBuf>,
    pub new_page_title: String,
    pub new_section_name: String,
    pub new_notebook_name: String,
    pub editing_block: Option<Id>,
    pub current_stroke: Vec<Point>,
    pub undo_stack: Vec<(Id, Edit)>,
    /// Cached RGBA8 pixels of decoded media, keyed by content hash.
    pub image_cache: HashMap<String, ImageData>,
}

/// Decoded RGBA8 pixels ready for canvas display.
#[derive(Clone, Debug)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Cached decoded media for an imported attachment ("open" button).
#[derive(Clone, Debug)]
pub struct MediaBlob {
    pub file_name: String,
    pub mime: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

/// Decode raw RGBA8 pixel data into a widget-drawable image handle.
pub fn rgba_image_handle(width: u32, height: u32, pixels: Vec<u8>) -> iced::widget::image::Handle {
    iced::widget::image::Handle::from_rgba(width, height, pixels)
}

impl AppState {
    pub fn new() -> Self {
        let config = Config::load();
        let mut st = Self {
            config,
            store: Store::new(),
            index: SearchIndex::new(),
            selection: None,
            rows: Vec::new(),
            query: String::new(),
            hits: Vec::new(),
            tool: Tool::Pen,
            color: Color::BLACK,
            width_mm: leafnote_core::canvas::default_width(PenKind::Pen),
            view: View::default(),
            dirty: false,
            status: String::from("Ready — open or create a notebook to begin"),
            last_report: None,
            pick_target: None,
            pending_open: None,
            new_page_title: String::new(),
            new_section_name: String::new(),
            new_notebook_name: String::new(),
            editing_block: None,
            current_stroke: Vec::new(),
            undo_stack: Vec::new(),
            image_cache: HashMap::new(),
        };
        if st.config.restore_session {
            let recents: Vec<PathBuf> = st.config.recent.clone();
            for dir in recents {
                if dir.is_dir() {
                    let _ = st.store.open_notebook(&dir);
                }
            }
            st.rebuild_tree();
            st.reindex_all();
        }
        st
    }

    // -- queries -----------------------------------------------------------

    pub fn current_notebook(&self) -> Option<Id> {
        match self.selection {
            Some(Selection::Notebook(n)) => Some(n),
            Some(Selection::Section(s)) | Some(Selection::Page { section: s, .. }) => {
                self.find_notebook_of_section(s)
            }
            None => self.store.notebooks().first().map(|n| n.id),
        }
    }

    pub fn find_notebook_of_section(&self, section_id: Id) -> Option<Id> {
        for nb in self.store.notebooks() {
            if contains_section(&nb.root, section_id) {
                return Some(nb.id);
            }
        }
        None
    }

    pub fn current_page_id(&self) -> Option<Id> {
        match self.selection {
            Some(Selection::Page { page, .. }) => Some(page),
            _ => None,
        }
    }

    pub fn current_section_id(&self) -> Option<Id> {
        match self.selection {
            Some(Selection::Page { section, .. }) | Some(Selection::Section(section)) => {
                Some(section)
            }
            _ => None,
        }
    }

    pub fn is_read_only(&self) -> bool {
        let Some(nb) = self.current_notebook() else { return false };
        let ro_nb = self
            .store
            .notebooks()
            .iter()
            .find(|n| n.id == nb)
            .map(|n| n.read_only)
            .unwrap_or(false);
        let ro_sec = self
            .current_section_id()
            .and_then(|s| self.store.section(s))
            .map(|s| s.read_only)
            .unwrap_or(false);
        ro_nb || ro_sec
    }

    pub fn page(&self) -> Option<&Page> {
        self.current_page_id().and_then(|p| self.store.page(p))
    }

    pub fn outlines(&self) -> Vec<(Id, Point, f32, Vec<Paragraph>)> {
        let Some(page) = self.page() else { return Vec::new() };
        page.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Outline { id, pos, width_mm, paragraphs } => {
                    Some((*id, *pos, *width_mm, paragraphs.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub fn attachments(&self) -> Vec<(Id, Point, String, u64)> {
        let Some(page) = self.page() else { return Vec::new() };
        page.blocks
            .iter()
            .filter_map(|b| match b {
                Block::File { id, pos, attachment } => {
                    Some((*id, *pos, attachment.file_name.clone(), attachment.size_bytes))
                }
                _ => None,
            })
            .collect()
    }

    pub fn all_strokes(&self) -> Vec<Stroke> {
        let Some(page) = self.page() else { return Vec::new() };
        let mut out = Vec::new();
        for b in &page.blocks {
            if let Block::Ink { strokes, .. } = b {
                out.extend(strokes.iter().cloned());
            }
        }
        out
    }

    pub fn images(&self) -> Vec<(Id, Point, String, f32, f32)> {
        let Some(page) = self.page() else { return Vec::new() };
        page.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { id, pos, image } => Some((
                    *id,
                    *pos,
                    image.media_hash.clone(),
                    image.width_mm,
                    image.height_mm,
                )),
                _ => None,
            })
            .collect()
    }

    pub fn title_of(&self, page_id: Id) -> String {
        self.store
            .page(page_id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| "(missing page)".into())
    }

    // -- mutations ----------------------------------------------------------

    pub fn select(&mut self, sel: Selection) {
        self.selection = Some(sel);
        self.editing_block = None;
        self.undo_stack.clear();
        self.view = View::default();
        self.rebuild_tree();
    }

    pub fn ensure_default_section(&mut self) -> Option<Id> {
        let nb = self.current_notebook()?;
        let empty = self
            .store
            .notebooks()
            .iter()
            .find(|n| n.id == nb)
            .map(|n| n.root.is_empty())
            .unwrap_or(true);
        if empty {
            let sec = self.store.create_section(nb, "Quick Notes").ok()?;
            self.store.create_page(sec, "Untitled page").ok()?;
            self.rebuild_tree();
            self.reindex_all();
            self.select(Selection::Page { section: sec, page: self.first_page_of(sec)? });
        }
        Some(nb)
    }

    fn first_page_of(&self, section: Id) -> Option<Id> {
        self.store.section(section).and_then(|s| s.page_ids.first().copied())
    }

    pub fn add_page(&mut self) {
        let Some(section) = self.current_section_id() else {
            self.status = "Select a section first".into();
            return;
        };
        if self.is_read_only() {
            self.status = "Read-only notebook — create a native notebook to author".into();
            return;
        }
        let title = if self.new_page_title.trim().is_empty() {
            "Untitled page".to_string()
        } else {
            self.new_page_title.trim().to_string()
        };
        match self.store.create_page(section, &title) {
            Ok(pid) => {
                self.new_page_title.clear();
                self.rebuild_tree();
                self.reindex_all();
                self.select(Selection::Page { section, page: pid });
                self.status = format!("Created page “{title}”");
            }
            Err(e) => self.status = format!("Could not create page: {e}"),
        }
    }

    pub fn add_section(&mut self) {
        let Some(nb) = self.current_notebook() else {
            self.status = "Open a notebook first".into();
            return;
        };
        let name = if self.new_section_name.trim().is_empty() {
            "New Section"
        } else {
            self.new_section_name.trim()
        };
        match self.store.create_section(nb, name) {
            Ok(sid) => {
                self.new_section_name.clear();
                let _ = self.store.create_page(sid, "Untitled page");
                self.rebuild_tree();
                self.status = format!("Created section “{name}”");
            }
            Err(e) => self.status = format!("Could not create section: {e}"),
        }
    }

    pub fn create_notebook(&mut self, root: &Path) {
        let name = if self.new_notebook_name.trim().is_empty() {
            "My Notebook"
        } else {
            self.new_notebook_name.trim()
        };
        match leafnote_core::store::create_notebook(root, name) {
            Ok(nb) => {
                let path = nb.path.clone();
                let id = nb.id;
                let _ = self.store.open_notebook(&path);
                self.config.push_recent(path);
                let _ = self.config.save();
                self.new_notebook_name.clear();
                self.rebuild_tree();
                self.selection = Some(Selection::Notebook(id));
                self.ensure_default_section();
                self.status = format!("Created notebook “{name}” in {}", root.display());
            }
            Err(e) => self.status = format!("Could not create notebook: {e}"),
        }
    }

    pub fn open_folder(&mut self, root: &Path) {
        match self.store.open_folder(root) {
            Ok(n) => {
                self.config.push_recent(root.to_path_buf());
                let _ = self.config.save();
                self.rebuild_tree();
                self.reindex_all();
                self.status = if n == 0 {
                    format!("No leafNote notebooks found in {}", root.display())
                } else {
                    format!("Opened {n} notebook(s) from {}", root.display())
                };
            }
            Err(e) => self.status = format!("Could not open folder: {e}"),
        }
    }

    pub fn import_onefile(&mut self, path: &Path) {
        let data_root = self.config.data_dir.clone();
        match import::import_any(&mut self.store, &data_root, path) {
            Ok((report, dir)) => {
                self.config.push_recent(dir);
                let _ = self.config.save();
                self.last_report = Some(report.clone());
                self.rebuild_tree();
                self.reindex_all();
                if let Some(nb) = report.notebook {
                    self.selection = Some(Selection::Notebook(nb));
                    self.ensure_default_section();
                }
                self.status = format!(
                    "Imported {} section(s), {} page(s), {} ink stroke(s), {} image(s), {} attachment(s) — originals untouched (read-only)",
                    report.sections, report.pages, report.ink_strokes, report.images, report.attachments
                );
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    pub fn rename_current_page(&mut self, title: String) {
        let Some(pid) = self.current_page_id() else { return };
        let Some(section) = self.current_section_id() else { return };
        if let Some(mut page) = self.store.page(pid).cloned() {
            page.title = title;
            page.touch();
            self.dirty = true;
            self.store.save_page(section, &page).ok();
            self.rebuild_tree();
            self.reindex_all();
        }
    }

    pub fn push_undo(&mut self, page: Id, edit: Edit) {
        self.undo_stack.push((page, edit));
        self.undo_stack.truncate(200);
    }

    pub fn undo(&mut self) {
        let Some((page_id, edit)) = self.undo_stack.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        let Some(section) = self.current_section_id() else { return };
        let Some(mut page) = self.store.page(page_id).cloned() else { return };
        match edit {
            Edit::AddBlock(b) => page.blocks.retain(|x| x.id() != b.id()),
            Edit::RemoveBlock(id) => {
                // Removal undo needs the stored block; kept simple: no-op if
                // we cannot recover it (blocks removed via eraser are gone).
                let _ = id;
            }
            Edit::UpdateBlock(b) => {
                if let Some(cur) = page.blocks.iter_mut().find(|x| x.id() == b.id()) {
                    *cur = b;
                }
            }
            Edit::RenamePage(t) => page.title = t,
        }
        page.touch();
        self.dirty = true;
        self.store.save_page(section, &page).ok();
        self.reindex_all();
        self.status = "Undo applied".into();
    }

    pub fn commit_stroke(&mut self) {
        if self.current_stroke.len() < 2 {
            self.current_stroke.clear();
            return;
        }
        let Some(page) = self.page().cloned() else {
            self.current_stroke.clear();
            return;
        };
        let pts = leafnote_core::canvas::decimate(&self.current_stroke, 0.15);
        self.current_stroke.clear();
        let kind = self.tool.pen_kind().unwrap_or(PenKind::Pen);
        let stroke = Stroke {
            points: pts,
            color: Color { a: 255, ..self.color },
            width_mm: self.width_mm,
            kind,
        };
        let mut page = page;
        if kind == PenKind::Eraser {
            self.erase_with(&stroke, &mut page);
        } else {
            let block = Block::Ink { id: Id::new(), strokes: vec![stroke] };
            self.push_undo(page.id, Edit::AddBlock(block.clone()));
            page.blocks.push(block);
        }
        page.touch();
        self.dirty = true;
        if let Some(section) = self.current_section_id() {
            self.store.save_page(section, &page).ok();
        }
    }

    fn erase_with(&mut self, stroke: &Stroke, page: &mut Page) {
        let bbox = leafnote_core::canvas::stroke_bounding_box(stroke);
        let tol = stroke.width_mm / 2.0 + 1.0;
        let mut removed: Vec<Block> = Vec::new();
        for b in &mut page.blocks {
            if let Block::Ink { strokes, .. } = b {
                strokes.retain(|s| !intersects(s, stroke, bbox, tol));
            }
        }
        page.blocks.retain(|b| match b {
            Block::Ink { strokes, .. } => {
                let empty = strokes.is_empty();
                if empty {
                    removed.push(b.clone());
                }
                !empty
            }
            _ => true,
        });
        for r in removed {
            self.push_undo(page.id, Edit::RemoveBlock(r.id()));
        }
    }

    pub fn edit_paragraph(&mut self, idx: usize, text: String) {
        let Some(block_id) = self.editing_block else { return };
        let Some(page) = self.page().cloned() else { return };
        let mut page = page;
        let old = page.blocks.iter().find(|b| b.id() == block_id).cloned();
        if let Some(Block::Outline { paragraphs, .. }) = old.as_ref() {
            let mut paragraphs = paragraphs.clone();
            if let Some(p) = paragraphs.get_mut(idx) {
                if p.text() != text {
                    p.runs = vec![leafnote_core::model::TextRun::plain(text)];
                    let nb = Block::Outline {
                        id: block_id,
                        pos: outline_pos(old.as_ref().unwrap()),
                        width_mm: outline_width(old.as_ref().unwrap()),
                        paragraphs,
                    };
                    if let Some(o) = old {
                        self.push_undo(page.id, Edit::UpdateBlock(o));
                    }
                    if let Some(cur) = page.blocks.iter_mut().find(|b| b.id() == block_id) {
                        *cur = nb;
                    }
                    page.touch();
                    self.dirty = true;
                    if let Some(section) = self.current_section_id() {
                        self.store.save_page(section, &page).ok();
                    }
                    self.reindex_all();
                }
            }
        }
    }

    pub fn toggle_checked(&mut self, idx: usize) {
        let Some(page) = self.page().cloned() else { return };
        let mut page = page;
        for b in &mut page.blocks {
            if let Block::Outline { paragraphs, .. } = b {
                if let Some(p) = paragraphs.get_mut(idx) {
                    if p.tag == Some(NoteTag::ToDo) {
                        p.checked_ = !p.checked_;
                        page.touch();
                        self.dirty = true;
                        if let Some(section) = self.current_section_id() {
                            self.store.save_page(section, &page).ok();
                        }
                        self.reindex_all();
                    }
                }
            }
        }
    }

    pub fn insert_image(&mut self, path: &Path) {
        let Some(bytes) = std::fs::read(path).ok() else {
            self.status = "Could not read image file".into();
            return;
        };
        let Some(section) = self.current_section_id() else {
            self.status = "Open a page first".into();
            return;
        };
        if self.is_read_only() {
            self.status = "Read-only section — cannot insert media".into();
            return;
        }
        let Some(nb_dir) = self.current_notebook().and_then(|n| self.store.notebook_dir(n)) else {
            return;
        };
        let hash = match Store::put_media(&nb_dir, &bytes) {
            Ok(h) => h,
            Err(e) => {
                self.status = format!("Media store failed: {e}");
                return;
            }
        };
        let (w, h) = decode_dims(&bytes);
        let mut page = unwrap_page(&self.store, self.current_page_id());
        page.blocks.push(Block::Image {
            id: Id::new(),
            pos: Point::new(20.0, 20.0),
            image: leafnote_core::model::ImageBlock {
                media_hash: hash.clone(),
                file_name: path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "image".into()),
                mime: guess_mime(path).to_string(),
                width_mm: w,
                height_mm: h,
                alt_text: None,
            },
        });
        page.touch();
        self.dirty = true;
        self.store.save_page(section, &page).ok();
        self.reindex_all();
        self.status = format!("Inserted image from {}", path.display());
    }

    pub fn insert_attachment(&mut self, path: &Path) {
        let Some(bytes) = std::fs::read(path).ok() else {
            self.status = "Could not read file".into();
            return;
        };
        let Some(section) = self.current_section_id() else {
            self.status = "Open a page first".into();
            return;
        };
        if self.is_read_only() {
            self.status = "Read-only section — cannot attach files".into();
            return;
        }
        let Some(nb_dir) = self.current_notebook().and_then(|n| self.store.notebook_dir(n)) else {
            return;
        };
        let hash = match Store::put_media(&nb_dir, &bytes) {
            Ok(h) => h,
            Err(e) => {
                self.status = format!("Media store failed: {e}");
                return;
            }
        };
        let mut page = unwrap_page(&self.store, self.current_page_id());
        page.blocks.push(Block::File {
            id: Id::new(),
            pos: Point::new(20.0, 20.0),
            attachment: leafnote_core::model::Attachment {
                media_hash: hash,
                file_name: leafnote_core::store::sanitize_folder_name(
                    &path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
                ),
                mime: guess_mime(path).to_string(),
                size_bytes: bytes.len() as u64,
            },
        });
        page.touch();
        self.dirty = true;
        self.store.save_page(section, &page).ok();
        self.reindex_all();
        self.status = format!("Attached {}", path.display());
    }

    pub fn add_text_block(&mut self, pos: Point) {
        let Some(page) = self.page().cloned() else { return };
        if self.is_read_only() {
            self.status = "Read-only page — select “Upgrade to editable” to edit".into();
            return;
        }
        let mut page = page;
        let id = page.add_text_block(pos);
        self.editing_block = Some(id);
        self.dirty = true;
        if let Some(section) = self.current_section_id() {
            self.store.save_page(section, &page).ok();
        }
    }

    pub fn upgrade_current_section(&mut self) {
        let Some(section) = self.current_section_id() else { return };
        let Some(mut sec) = self.store.section(section).cloned() else { return };
        sec.read_only = false;
        let Some(nb) = self.current_notebook() else { return };
        if let Some(nb_obj) = self.store.notebook_mut(nb) {
            set_section_ro(&mut nb_obj.root, section, false);
        }
        let _ = self.store.register_section(nb, sec);
        self.rebuild_tree();
        self.status = "Section upgraded — edits now persist to your local notebook".into();
    }

    pub fn save_if_dirty(&mut self) {
        if let Some(section) = self.current_section_id() {
            if let Some(page) = self.page().cloned() {
                let _ = self.store.save_page(section, &page);
            }
        }
        self.dirty = false;
        self.reindex_all();
    }

    pub fn run_search(&mut self) {
        self.hits = self.index.search(&self.query, 50);
    }

    pub fn reindex_all(&mut self) {
        self.index.clear();
        let notebooks: Vec<(Id, Vec<TreeNode>)> = self
            .store
            .notebooks()
            .iter()
            .map(|n| (n.id, n.root.clone()))
            .collect();
        for (nb_id, tree) in notebooks {
            let pairs = collect_sections(&tree);
            for (sec_id, name, path) in pairs {
                if let Ok(pages) = self.store.section_pages(sec_id) {
                    for p in pages {
                        self.index.index_page(&p, nb_id, sec_id, &name, &path);
                    }
                }
            }
        }
    }

    pub fn rebuild_tree(&mut self) {
        let mut rows = Vec::new();
        for nb in self.store.notebooks() {
            let sel = self.selection == Some(Selection::Notebook(nb.id));
            rows.push(TreeRow {
                key: format!("nb{}", nb.id),
                depth: 0,
                label: nb.name.clone(),
                icon: "📓",
                selected: sel,
                read_only: nb.read_only,
                sel: Selection::Notebook(nb.id),
            });
            walk_tree(&nb.root, 1, self.selection, &mut rows, self);
        }
        self.rows = rows;
    }

    /// Pan the camera by a screen-pixel delta.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.view.pan_x += dx;
        self.view.pan_y += dy;
    }

    /// Zoom keeping the screen point (x, y) fixed under the cursor.
    pub fn zoom_at(&mut self, x: f32, y: f32, factor: f32) {
        let old = self.view.zoom;
        let new = (old * factor).clamp(0.4, 12.0);
        if (new - old).abs() < f32::EPSILON {
            return;
        }
        self.view.pan_x = x - (x - self.view.pan_x) * (new / old);
        self.view.pan_y = y - (y - self.view.pan_y) * (new / old);
        self.view.zoom = new;
    }

    /// Decode + cache any images on the current page that are not yet cached.
    pub fn ensure_images_cached(&mut self) {
        let Some(nb) = self.current_notebook() else { return };
        let Some(dir) = self.store.notebook_dir(nb) else { return };
        let hashes: Vec<String> = self
            .images()
            .into_iter()
            .map(|(_, _, h, _, _)| h)
            .filter(|h| !self.image_cache.contains_key(h))
            .collect();
        for hash in hashes {
            if let Ok(bytes) = Store::get_media(&dir, &hash) {
                if let Some(img) = decode_rgba(&bytes) {
                    self.image_cache.insert(hash, img);
                }
            }
        }
    }

    /// Close a notebook (removes it from the session; files stay on disk).
    pub fn close_notebook(&mut self, id: Id) {
        self.store.close_notebook(id);
        self.selection = None;
        self.rebuild_tree();
        self.reindex_all();
        self.status = "Notebook closed (files untouched)".into();
    }

    pub fn apply_tool(&mut self, t: Tool) {
        self.tool = t;
        if let Some(k) = t.pen_kind() {
            self.width_mm = leafnote_core::canvas::default_width(k);
        }
    }
}

fn walk_tree(
    nodes: &[TreeNode],
    depth: usize,
    selection: Option<Selection>,
    rows: &mut Vec<TreeRow>,
    state: &AppState,
) {
    for node in nodes {
        match node {
            TreeNode::Section(s) => {
                let sel = matches!(selection, Some(Selection::Section(id)) if id == s.id);
                rows.push(TreeRow {
                    key: format!("s{}", s.id),
                    depth,
                    label: s.name.clone(),
                    icon: "📂",
                    selected: sel,
                    read_only: s.read_only,
                    sel: Selection::Section(s.id),
                });
                // Expand the section that holds the current page selection.
                if let Some(Selection::Page { section, page }) = selection {
                    if section == s.id {
                        let pids: Vec<Id> = state
                            .store
                            .section(s.id)
                            .map(|sec| sec.page_ids.clone())
                            .unwrap_or_default();
                        for pid in &pids {
                            let is_cur = *pid == page;
                            rows.push(TreeRow {
                                key: format!("p{pid}"),
                                depth: depth + 1,
                                label: state.title_of(*pid),
                                icon: if is_cur { "📝" } else { "📄" },
                                selected: matches!(selection, Some(Selection::Page { page: p, .. }) if p == *pid),
                                read_only: s.read_only,
                                sel: Selection::Page { section, page: *pid },
                            });
                        }
                    }
                }
            }
            TreeNode::Group(g) => {
                rows.push(TreeRow {
                    key: format!("g{}", g.id),
                    depth,
                    label: g.name.clone(),
                    icon: "🗂",
                    selected: false,
                    read_only: false,
                    // Groups are display-only containers; selecting one just
                    // keeps its notebook active.
                    sel: match selection {
                        Some(s) => s,
                        None => Selection::Notebook(state.store.notebooks().first().map(|n| n.id).unwrap_or(g.id)),
                    },
                });
                walk_tree(&g.children, depth + 1, selection, rows, state);
            }
        }
    }
}

fn contains_section(nodes: &[TreeNode], id: Id) -> bool {
    nodes.iter().any(|n| match n {
        TreeNode::Section(s) => s.id == id,
        TreeNode::Group(g) => contains_section(&g.children, id),
    })
}

fn set_section_ro(nodes: &mut [TreeNode], id: Id, ro: bool) {
    for n in nodes.iter_mut() {
        match n {
            TreeNode::Section(s) if s.id == id => s.read_only = ro,
            TreeNode::Group(g) => set_section_ro(&mut g.children, id, ro),
            _ => {}
        }
    }
}

fn collect_sections(tree: &[TreeNode]) -> Vec<(Id, String, Vec<String>)> {
    fn walk(nodes: &[TreeNode], path: &mut Vec<String>, out: &mut Vec<(Id, String, Vec<String>)>) {
        for n in nodes {
            match n {
                TreeNode::Section(s) => out.push((s.id, s.name.clone(), path.clone())),
                TreeNode::Group(g) => {
                    path.push(g.name.clone());
                    walk(&g.children, path, out);
                    path.pop();
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut Vec::new(), &mut out);
    out
}

fn outline_pos(b: &Block) -> Point {
    match b {
        Block::Outline { pos, .. } => *pos,
        _ => Point::default(),
    }
}
fn outline_width(b: &Block) -> f32 {
    match b {
        Block::Outline { width_mm, .. } => *width_mm,
        _ => 120.0,
    }
}

fn intersects(s: &Stroke, eraser: &Stroke, ebox: leafnote_core::model::Rect, tol: f32) -> bool {
    let bb = leafnote_core::canvas::stroke_bounding_box(s);
    if !(bb.x <= ebox.x + ebox.w && ebox.x <= bb.x + bb.w && bb.y <= ebox.y + ebox.h && ebox.y <= bb.y + bb.h) {
        return false;
    }
    s.points.iter().any(|p| {
        eraser
            .points
            .windows(2)
            .any(|w| dist_point_seg(*p, w[0], w[1]) <= tol + s.width_mm / 2.0)
            || (eraser.points.len() == 1 && p.distance(eraser.points[0]) <= tol)
    })
}

fn dist_point_seg(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * dx, a.y + t * dy))
}

fn unwrap_page(store: &Store, id: Option<Id>) -> Page {
    id.and_then(|i| store.page(i).cloned()).unwrap_or_else(|| Page::new("Untitled page"))
}

fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Decode image dimensions without a full image crate dependency: parse the
/// IHDR/SOF/OCH headers directly (PNG/JPEG/GIF). Fall back to A4-ish defaults.
pub fn decode_dims(bytes: &[u8]) -> (f32, f32) {
    let (px_w, px_h) = raw_dims(bytes).unwrap_or((800, 600));
    // assume ~96 dpi → mm
    let w = px_w as f32 / 96.0 * 25.4;
    let h = px_h as f32 / 96.0 * 25.4;
    (w.min(300.0), h.min(400.0))
}

fn raw_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    if bytes.starts_with(b"\xff\xd8") {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            i += 2 + len;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Minimal PNG decoding for canvas display (8-bit RGB/RGBA, non-interlaced).
// ---------------------------------------------------------------------------

/// Decode a PNG's IDAT into raw RGBA8. Returns `None` for unsupported
/// variants (16-bit, palette, interlaced) or non-PNG input — callers then
/// render a placeholder instead.
pub fn decode_rgba(bytes: &[u8]) -> Option<ImageData> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut pos = 8usize;
    let mut ihdr: Option<(u32, u32, u8)> = None;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= bytes.len() {
        let len =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len)?;
        if data_end + 4 > bytes.len() {
            return None;
        }
        match kind {
            b"IHDR" => {
                let w = u32::from_be_bytes([
                    bytes[data_start],
                    bytes[data_start + 1],
                    bytes[data_start + 2],
                    bytes[data_start + 3],
                ]);
                let h = u32::from_be_bytes([
                    bytes[data_start + 4],
                    bytes[data_start + 5],
                    bytes[data_start + 6],
                    bytes[data_start + 7],
                ]);
                let bd = bytes[data_start + 8];
                let ct = bytes[data_start + 9];
                let cm = bytes[data_start + 10];
                let il = bytes[data_start + 12];
                if bd != 8 || cm != 0 || il != 0 {
                    return None;
                }
                ihdr = Some((w, h, ct));
            }
            b"IDAT" => idat.extend_from_slice(&bytes[data_start..data_end]),
            b"IEND" => break,
            _ => {}
        }
        pos = data_end + 4;
    }
    let (w, h, ct) = ihdr?;
    let channels: usize = match ct {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let raw = inflate(&idat)?;
    let bpp = channels;
    let stride = (w as usize) * bpp;
    let expected = stride * h as usize + h as usize; // + filter byte per row
    if raw.len() < expected {
        return None;
    }
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut prev = vec![0u8; stride];
    let mut rp = 0usize;
    for y in 0..h as usize {
        let ft = raw[rp];
        rp += 1;
        let line = &raw[rp..rp + stride];
        rp += stride;
        let cur = unfilter(ft, line, &prev, bpp);
        let row_out = y * (w as usize) * 4;
        for x in 0..w as usize {
            let o = row_out + x * 4;
            match channels {
                1 => {
                    let v = cur[x];
                    out[o] = v;
                    out[o + 1] = v;
                    out[o + 2] = v;
                    out[o + 3] = 255;
                }
                2 => {
                    out[o] = cur[x * 3];
                    out[o + 1] = cur[x * 3 + 1];
                    out[o + 2] = cur[x * 3 + 2];
                    out[o + 3] = 255;
                }
                3 => {
                    out[o] = cur[x * 2];
                    out[o + 1] = cur[x * 2];
                    out[o + 2] = cur[x * 2];
                    out[o + 3] = cur[x * 2 + 1];
                }
                _ => out[o..o + 4].copy_from_slice(&cur[x * 4..x * 4 + 4]),
            }
        }
        prev = cur;
    }
    Some(ImageData { width: w, height: h, pixels: out })
}

fn unfilter(ft: u8, line: &[u8], prev: &[u8], bpp: usize) -> Vec<u8> {
    let n = line.len();
    let mut cur = vec![0u8; n];
    for i in 0..n {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let f = line[i] as i32;
        cur[i] = match ft {
            0 => line[i],
            1 => (f + a as i32) as u8,
            2 => (f + b as i32) as u8,
            3 => (f + ((a as i32 + b as i32) / 2)) as u8,
            4 => {
                let p = a as i32 + b as i32 - c as i32;
                let pa = (p - a as i32).abs();
                let pb = (p - b as i32).abs();
                let pc = (p - c as i32).abs();
                let pr = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                (f + pr as i32) as u8
            }
            _ => line[i],
        };
    }
    cur
}

/// Inflate raw DEFLATE data (stored, fixed, and dynamic Huffman blocks).
/// Dependency-free; sufficient for PNG IDAT streams.
pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let bfinal = br.bit()?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align_byte();
                let len = br.u16_le()?;
                let _nlen = br.u16_le()?;
                for _ in 0..len {
                    out.push(br.byte()?);
                }
            }
            1 => {
                let lit = FixedHuff::lit_len();
                let dist = FixedHuff::dist();
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = read_dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return None,
        }
        if bfinal {
            break;
        }
    }
    Some(out)
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, bitbuf: 0, bitcnt: 0 }
    }
    fn fill(&mut self) -> Option<()> {
        while self.bitcnt <= 24 {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.bitbuf |= (b as u32) << self.bitcnt;
            self.bitcnt += 8;
        }
        Some(())
    }
    fn bit(&mut self) -> Option<u32> {
        self.fill()?;
        let b = self.bitbuf & 1;
        self.bitbuf >>= 1;
        self.bitcnt -= 1;
        Some(b)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        self.fill()?;
        let v = self.bitbuf & ((1u32 << n) - 1);
        self.bitbuf >>= n;
        self.bitcnt -= n;
        Some(v)
    }
    fn align_byte(&mut self) {
        let drop = self.bitcnt % 8;
        self.bitbuf >>= drop;
        self.bitcnt -= drop;
    }
    fn byte(&mut self) -> Option<u8> {
        self.fill()?;
        let v = (self.bitbuf & 0xFF) as u8;
        self.bitbuf >>= 8;
        self.bitcnt -= 8;
        Some(v)
    }
    fn u16_le(&mut self) -> Option<u16> {
        let lo = self.byte()? as u16;
        let hi = self.byte()? as u16;
        Some(lo | (hi << 8))
    }
}

/// Canonical Huffman table built from code lengths.
struct HuffTable {
    counts: Vec<u16>,
    symbols: Vec<u16>,
}

impl HuffTable {
    fn from_lengths(lengths: &[u8]) -> Option<Self> {
        let maxbits = 15usize;
        let mut counts = vec![0u16; maxbits + 1];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = vec![0u16; maxbits + 2];
        for i in 1..maxbits {
            offs[i + 1] = offs[i] + counts[i];
        }
        let total = (offs[maxbits] + counts[maxbits]) as usize;
        let mut symbols = vec![0u16; total];
        let mut next = offs.clone();
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[next[l as usize] as usize] = sym as u16;
                next[l as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }
    fn decode(&self, br: &mut BitReader) -> Option<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=15usize {
            code |= br.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Some(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

struct FixedHuff;
impl FixedHuff {
    fn lit_len() -> HuffTable {
        let mut l = [0u8; 288];
        for (i, slot) in l.iter_mut().enumerate() {
            *slot = match i {
                0..144 => 8,
                144..256 => 9,
                256..280 => 7,
                _ => 8,
            };
        }
        HuffTable::from_lengths(&l).unwrap()
    }
    fn dist() -> HuffTable {
        let l = [5u8; 30];
        HuffTable::from_lengths(&l).unwrap()
    }
}

fn read_dynamic_tables(br: &mut BitReader) -> Option<(HuffTable, HuffTable)> {
    let hlit = br.bits(5)? + 257;
    let hdist = br.bits(5)? + 1;
    let hclen = br.bits(4)? + 4;
    const ORDER: [usize; 19] =
        [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut clen = [0u8; 19];
    for i in 0..hclen as usize {
        clen[ORDER[i]] = br.bits(3)? as u8;
    }
    let cltable = HuffTable::from_lengths(&clen)?;
    let mut lengths = vec![0u8; (hlit + hdist) as usize];
    let mut i = 0usize;
    while (i as u32) < hlit + hdist {
        let sym = cltable.decode(br)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                let rep = br.bits(2)? + 3;
                if i == 0 {
                    return None;
                }
                let prev = lengths[i - 1];
                for _ in 0..rep {
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let rep = br.bits(3)? + 3;
                for _ in 0..rep {
                    lengths[i] = 0;
                    i += 1;
                }
            }
            18 => {
                let rep = br.bits(7)? + 11;
                for _ in 0..rep {
                    lengths[i] = 0;
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    let lit = HuffTable::from_lengths(&lengths[..hlit as usize])?;
    let dist = HuffTable::from_lengths(&lengths[hlit as usize..])?;
    Some((lit, dist))
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51,
    59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4,
    4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385,
    513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9,
    10, 10, 11, 11, 12, 12, 13, 13];

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &HuffTable,
    dist: &HuffTable,
) -> Option<()> {
    loop {
        let sym = lit.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Some(()),
            257..=285 => {
                let idx = (sym - 257) as usize;
                let len = LEN_BASE[idx] + br.bits(LEN_EXTRA[idx] as u32)? as u16;
                let dsym = dist.decode(br)?;
                if dsym >= 30 {
                    return None;
                }
                let didx = dsym as usize;
                let d = (DIST_BASE[didx] + br.bits(DIST_EXTRA[didx] as u32)? as u16) as usize;
                if d > out.len() {
                    return None;
                }
                let start = out.len() - d;
                for i in 0..len as usize {
                    let b = out[start + i % d];
                    out.push(b);
                }
            }
            _ => return None,
        }
    }
}
