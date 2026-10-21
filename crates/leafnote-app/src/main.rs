//! leafNote — a local-first, offline OneNote-style notebook written in pure
//! Rust. This module is the Iced 0.13 view layer: a three-pane layout with a
//! notebook-tree sidebar, an infinite freeform canvas (ink + text + media),
//! and an inspector panel for tools, tags, search results, and page info.
//!
//! All application logic lives in [`state`] (which bridges to the
//! `leafnote-core` engine); this file only *renders* that state and maps user
//! interaction back into it.

#![windows_subsystem = "windows"]

mod state;

use std::path::{Path, PathBuf};

use iced::widget::canvas::{
    self, Event, Frame, Geometry, Path as CPath, Program, Stroke as CStroke, Text as CText,
};
use iced::widget::{
    button, column, container, horizontal_space, row, scrollable, text, text_input,
    vertical_space, Column, Space,
};
use iced::{theme, Element, Length, Size, Task, Theme};

/// Canvas-local event state kept by the `Program` implementation.
type DrawState = canvas::event::State;

use leafnote_core::model::{Block, Color, Id, NoteTag, PenKind, Point};
use leafnote_core::search::SearchHit;

use state::{AppState, CanvasMsg, PickTarget, Selection, Tool, TreeRow};

const APP_TITLE: &str = "leafNote — local-first notebooks";

fn main() -> iced::Result {
    let startup_file: Option<PathBuf> = std::env::args().nth(1).map(PathBuf::from);
    iced::application(App::title, App::update, App::view)
        .subscription(App::subscription)
        .run_with(move || {
            let mut app = App::new();
            if let Some(p) = startup_file {
                if p.is_dir() {
                    app.state.open_folder(&p);
                } else {
                    app.state.import_onefile(&p);
                }
            }
            (app, Task::none())
        })
}

pub struct App {
    pub state: AppState,
}

impl App {
    /// Autosave heartbeat: flush dirty state every 5 s (OneNote-like).
    fn on_tick(app: &mut App, _now: std::time::Instant) -> Vec<Msg> {
        app.state.save_if_dirty();
        vec![Msg::Tick]
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Tick,
    RequestPick(PickTarget),
    Picked(PickTarget, Option<PathBuf>),
    RowSelected(Selection),
    SetTool(Tool),
    SetColor(Color),
    SetWidth(f32),
    Query(String),
    RunSearch,
    OpenHit(SearchHit),
    NewPageTitle(String),
    AddPage,
    NewSectionName(String),
    AddSection,
    NewNotebookName(String),
    CreateNotebook,
    RenamePage(String),
    UpgradeSection,
    Undo,
    SaveNow,
    CloseNotebook(Id),
    CanvasEvent(CanvasMsg),
    ZoomIn,
    ZoomOut,
    ResetView,
}

impl App {
    fn new() -> Self {
        Self { state: AppState::new() }
    }

    /// Launch any dialog the state has queued up.
    fn pump(&mut self) -> Task<Msg> {
        use rfd::AsyncFileDialog;
        match self.state.pick_target.take() {
            Some(t @ PickTarget::OpenOneNote) => Task::perform(
                AsyncFileDialog::new()
                    .set_title("Open OneNote file")
                    .add_filter("OneNote files", &["one", "onetoc2", "onepkg"])
                    .pick_file(),
                move |f| Msg::Picked(t, f.map(|p| p.path().to_path_buf())),
            ),
            Some(t @ PickTarget::InsertImage) => Task::perform(
                AsyncFileDialog::new()
                    .set_title("Insert image")
                    .add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp"])
                    .pick_file(),
                move |f| Msg::Picked(t, f.map(|p| p.path().to_path_buf())),
            ),
            Some(t @ PickTarget::InsertFile) => Task::perform(
                AsyncFileDialog::new().set_title("Attach a file").pick_file(),
                move |f| Msg::Picked(t, f.map(|p| p.path().to_path_buf())),
            ),
            None => Task::none(),
        }
    }

    fn title(&self, _s: &App) -> String {
        APP_TITLE.to_string()
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Tick => {}
            Msg::RequestPick(target) => {
                self.state.pick_target = Some(target);
                return self.pump();
            }
            Msg::Picked(target, Some(path)) => match target {
                PickTarget::OpenOneNote => self.state.import_onefile(&path),
                PickTarget::InsertImage => self.state.insert_image(&path),
                PickTarget::InsertFile => self.state.insert_attachment(&path),
            },
            Msg::Picked(_, None) => {}
            Msg::RowSelected(sel) => self.state.select(sel),
            Msg::SetTool(t) => self.state.apply_tool(t),
            Msg::SetColor(c) => self.state.color = c,
            Msg::SetWidth(w) => self.state.width_mm = w.clamp(0.2, 12.0),
            Msg::Query(q) => {
                self.state.query = q;
                self.state.run_search();
            }
            Msg::RunSearch => self.state.run_search(),
            Msg::OpenHit(hit) => {
                self.state
                    .select(Selection::Page { section: hit.section_id, page: hit.page_id });
            }
            Msg::NewPageTitle(t) => self.state.new_page_title = t,
            Msg::AddPage => self.state.add_page(),
            Msg::NewSectionName(n) => self.state.new_section_name = n,
            Msg::AddSection => self.state.add_section(),
            Msg::NewNotebookName(n) => self.state.new_notebook_name = n,
            Msg::CreateNotebook => {
                let root = leafnote_core::config::default_data_dir();
                self.state.create_notebook(&root);
            }
            Msg::RenamePage(t) => self.state.rename_current_page(t),
            Msg::UpgradeSection => self.state.upgrade_current_section(),
            Msg::Undo => self.state.undo(),
            Msg::SaveNow => {
                self.state.save_if_dirty();
                self.state.status = "Saved to disk ✓".into();
            }
            Msg::CloseNotebook(id) => self.state.close_notebook(id),
            Msg::CanvasEvent(ev) => match ev {
                CanvasMsg::DragStart(p) => {
                    if matches!(self.state.tool, Tool::Text) {
                        self.state.add_text_block(p);
                    } else {
                        self.state.current_stroke = vec![p];
                    }
                }
                CanvasMsg::DragPoint(p) => {
                    if !matches!(self.state.tool, Tool::Text | Tool::Select) {
                        self.state.current_stroke.push(p);
                    }
                }
                CanvasMsg::DragEnd => self.state.commit_stroke(),
                CanvasMsg::PanBy(dx, dy) => self.state.pan_by(dx, dy),
                CanvasMsg::ZoomAt(x, y, factor) => self.state.zoom_at(x, y, factor),
                CanvasMsg::ParaEdited(idx, txt) => self.state.edit_paragraph(idx, txt),
                CanvasMsg::ToggleCheck(idx) => self.state.toggle_checked(idx),
                CanvasMsg::OpenAttachment(id) => self.open_attachment(id),
            },
            Msg::ZoomIn => {
                self.state.view.zoom = (self.state.view.zoom * 1.25).min(12.0);
            }
            Msg::ZoomOut => {
                self.state.view.zoom = (self.state.view.zoom / 1.25).max(0.4);
            }
            Msg::ResetView => self.state.view = state::View::default(),
        }
        self.state.ensure_images_cached();
        Task::none()
    }

    fn open_attachment(&mut self, id: Id) {
        let Some((hash, name)) = self.state.page().and_then(|p| {
            p.blocks.iter().find_map(|b| match b {
                Block::File { id: i, attachment, .. } if *i == id => {
                    Some((attachment.media_hash.clone(), attachment.file_name.clone()))
                }
                _ => None,
            })
        }) else {
            self.state.status = "Attachment not found".into();
            return;
        };
        let Some(dir) = self
            .state
            .current_notebook()
            .and_then(|n| self.state.store.notebook_dir(n))
        else {
            return;
        };
        match leafnote_core::store::Store::get_media(&dir, &hash) {
            Ok(bytes) => {
                let out = leafnote_core::config::config_dir()
                    .join("cache")
                    .join(leafnote_core::store::sanitize_folder_name(&name));
                let _ = std::fs::create_dir_all(out.parent().unwrap());
                if std::fs::write(&out, &bytes).is_ok() {
                    open_in_os(&out);
                    self.state.status = format!("Opened “{name}” with the system handler");
                } else {
                    self.state.status = "Could not write temp copy of attachment".into();
                }
            }
            Err(e) => self.state.status = format!("Could not read media: {e}"),
        }
    }

    fn view(&self, app: &App) -> Element<'_, Msg> {
        let st = &app.state;
        let header = header_view(st);
        let body = row![
            sidebar_view(st).width(Length::Fixed(290.0)),
            vertical_line(),
            canvas_column(st).fill(),
            vertical_line(),
            inspector_view(st).width(Length::Fixed(310.0)),
        ];
        let footer = status_bar(st);
        container(column![header, body.height(Length::Fill), footer].spacing(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> iced::Subscription<Msg> {
        // Autosave heartbeat: flush dirty state every 5 s (OneNote-like).
        iced::time::every(std::time::Duration::from_millis(5_000)).map(|_| Msg::Tick)
    }
}

/// Best-effort cross-platform "open with default handler".
fn open_in_os(path: &Path) {
    let p = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&p).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &p])
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(&p).spawn();
}

// ===========================================================================
// Header / toolbar
// ===========================================================================

fn header_view(st: &AppState) -> Element<'_, Msg> {
    let tool_btn = |t: Tool, label: &'static str| {
        button(text(label).size(13))
            .padding([4, 10])
            .style(move |th: &Theme, status| {
                let base = theme::Button::primary(th, status);
                if st.tool == t {
                    iced::widget::button::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb(
                            0.16, 0.42, 0.85,
                        ))),
                        text_color: Some(iced::Color::WHITE),
                        ..base
                    }
                } else {
                    base
                }
            })
            .on_press(Msg::SetTool(t))
    };

    let ro_badge: Element<'_, Msg> = if st.is_read_only() && st.page().is_some() {
        container(text("🔒 read-only import").size(12))
            .padding([2, 8])
            .style(|_: &Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgb(0.97, 0.82, 0.35))),
                text_color: Some(iced::Color::BLACK),
                border: iced::Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    } else {
        Space::with_width(Length::Shrink).into()
    };

    let save_state = if st.dirty { "● unsaved changes" } else { "✓ all changes saved" };

    row![
        text("🍃 leafNote").size(20),
        Space::with_width(12),
        tool_btn(Tool::Select, "↖ Select"),
        tool_btn(Tool::Text, "T Text"),
        tool_btn(Tool::Pen, "✒ Pen"),
        tool_btn(Tool::Pencil, "✏ Pencil"),
        tool_btn(Tool::Highlighter, "🖍 Highlight"),
        tool_btn(Tool::Eraser, "⌫ Erase"),
        Space::with_width(12),
        button(text("－").size(15)).padding([3, 8]).on_press(Msg::ZoomOut),
        button(text("＋").size(15)).padding([3, 8]).on_press(Msg::ZoomIn),
        button(text("⤢ Fit").size(12)).padding([3, 8]).on_press(Msg::ResetView),
        Space::with_width(Length::Fill),
        ro_badge,
        Space::with_width(12),
        text(save_state).size(12),
        Space::with_width(12),
        button(text("💾 Save").size(13)).padding([4, 10]).on_press(Msg::SaveNow),
    ]
    .padding([8, 12])
    .align_y(iced::Align::Center)
    .spacing(4)
    .into()
}

fn vertical_line() -> Element<'static, Msg> {
    container(Space::new(Length::Fixed(1.0), Length::Fill))
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb(0.82, 0.83, 0.86))),
            ..Default::default()
        })
        .into()
}

// ===========================================================================
// Sidebar: notebook tree + creation controls + import
// ===========================================================================

fn sidebar_view(st: &AppState) -> Element<'_, Msg> {
    let mut col = Column::new().spacing(6).padding(10);

    col = col.push(text("Notebooks").size(16));

    col = col.push(
        text_input("New notebook name…", &st.new_notebook_name)
            .on_input(Msg::NewNotebookName)
            .on_submit(Msg::CreateNotebook)
            .padding(6)
            .size(13),
    );
    col = col.push(
        row![
            button(text("🆕 Create").size(12)).padding([4, 8]).on_press(Msg::CreateNotebook),
            button(text("📥 Import .one / .onetoc2 / .onepkg").size(12))
                .padding([4, 8])
                .on_press(Msg::RequestPick(PickTarget::OpenOneNote)),
        ]
        .spacing(6),
    );

    // -- tree --------------------------------------------------------------
    let tree: Column<Msg> = if st.rows.is_empty() {
        column![
            text("No notebooks yet.").size(13),
            text("Create one above, or import a OneNote backup (.one/.onetoc2/.onepkg).")
                .size(12)
                .color(iced::Color::from_rgb(0.45, 0.45, 0.5)),
        ]
    } else {
        let mut c = Column::new().spacing(2);
        for r in &st.rows {
            c = c.push(tree_row(r));
        }
        c
    };
    col = col.push(scrollable(tree).height(Length::FillPortion(3)));

    // -- section/page creation --------------------------------------------
    match st.selection {
        Some(Selection::Notebook(nb)) => {
            col = col.push(
                text_input("New section name…", &st.new_section_name)
                    .on_input(Msg::NewSectionName)
                    .on_submit(Msg::AddSection)
                    .padding(6)
                    .size(13),
            );
            col = col.push(
                row![
                    button(text("＋ Add section").size(12)).padding([4, 8]).on_press(Msg::AddSection),
                    button(text("✕ Close notebook").size(12))
                        .padding([4, 8])
                        .on_press(Msg::CloseNotebook(nb)),
                ]
                .spacing(6),
            );
        }
        Some(Selection::Section(_)) | Some(Selection::Page { .. }) => {
            col = col.push(
                text_input("New page title…", &st.new_page_title)
                    .on_input(Msg::NewPageTitle)
                    .on_submit(Msg::AddPage)
                    .padding(6)
                    .size(13),
            );
            col =
                col.push(button(text("＋ Add page").size(12)).padding([4, 8]).on_press(Msg::AddPage));
        }
        None => {}
    }

    // -- import report -----------------------------------------------------
    if let Some(rep) = &st.last_report {
        col = col.push(container(
            column![
                text("Import summary").size(13),
                text(format!(
                    "{} sections · {} pages\n{} ink strokes · {} images\n{} attachments",
                    rep.sections, rep.pages, rep.ink_strokes, rep.images, rep.attachments
                ))
                .size(12),
            ]
            .spacing(2),
        )
        .padding(8)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgb(0.93, 0.96, 0.92))),
            border: iced::Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        }));
    }

    container(col).height(Length::Fill).into()
}

fn tree_row(r: &TreeRow) -> Element<'static, Msg> {
    let indent = r.depth as u16 * 14;
    let label = format!(
        "{} {}{}",
        r.icon,
        r.label,
        if r.read_only { "  🔒" } else { "" }
    );
    let selected = r.selected;
    let inner = container(text(label).size(13))
        .width(Length::Fill)
        .padding([3, 6])
        .style(move |_: &Theme| {
            let bg = if selected {
                iced::Color::from_rgb(0.80, 0.88, 1.0)
            } else {
                iced::Color::TRANSPARENT
            };
            container::Style {
                background: Some(iced::Background::Color(bg)),
                border: iced::Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        });
    container(
        button(inner)
            .width(Length::Fill)
            .style(|_: &Theme, _: iced::widget::button::Status| iced::widget::button::Style {
                background: None,
                text_color: None,
                border: Default::default(),
                shadow: Default::default(),
            })
            .on_press(Msg::RowSelected(r.sel)),
    )
    .width(Length::Fill)
    .padding([0, 0, 0, indent])
    .into()
}

// ===========================================================================
// Canvas pane
// ===========================================================================

fn canvas_column(st: &AppState) -> Element<'_, Msg> {
    let top = match st.page() {
        Some(p) => row![
            text_input("Page title", &p.title)
                .on_input(Msg::RenamePage)
                .padding(6)
                .size(16),
            Space::with_width(Length::Fill),
            text(format!("modified {}", p.modified.format("%Y-%m-%d %H:%M")))
                .size(11)
                .color(iced::Color::from_rgb(0.45, 0.45, 0.5)),
        ]
        .spacing(10)
        .align_y(iced::Align::Center),
        None => row![text("No page selected — pick or create a page in the sidebar")
            .size(14)
            .color(iced::Color::from_rgb(0.5, 0.5, 0.5))]
            .align_y(iced::Align::Center),
    };

    let canvas = canvas::Canvas::new(Draw { st }).width(Length::Fill).height(Length::Fill);

    container(column![top.padding(8), canvas].spacing(0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

struct Draw<'a> {
    st: &'a AppState,
}

#[derive(Default)]
struct DrawState {
    drawing: bool,
    panning: bool,
    last: (f32, f32),
}

impl<'a> Program<Msg, DrawState> for Draw<'a> {
    fn update(
        &self,
        ds: &mut DrawState,
        event: Event,
        bounds: iced::Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<Msg>) {
        let _ = bounds;
        let select_mode = matches!(self.st.tool, Tool::Select);

        match event {
            Event::Mouse(iced::event::mouse::Event::ButtonPressed(
                iced::event::mouse::Button::Left,
                _,
            )) => {
                let Some(pos) = cursor.position() else {
                    return (canvas::event::Status::Ignored, None);
                };
                ds.last = (pos.x, pos.y);
                let world = Point::new(
                    (pos.x - self.st.view.pan_x) / self.st.view.zoom,
                    (pos.y - self.st.view.pan_y) / self.st.view.zoom,
                );
                if select_mode {
                    ds.panning = true;
                    (canvas::event::Status::Captured, None)
                } else {
                    ds.drawing = true;
                    (
                        canvas::event::Status::Captured,
                        Some(Msg::CanvasEvent(CanvasMsg::DragStart(world))),
                    )
                }
            }
            Event::Mouse(iced::event::mouse::Event::CursorMoved { position, .. }) => {
                if ds.panning {
                    let dx = position.x - ds.last.0;
                    let dy = position.y - ds.last.1;
                    ds.last = (position.x, position.y);
                    return (
                        canvas::event::Status::Captured,
                        Some(Msg::CanvasEvent(CanvasMsg::PanBy(dx, dy))),
                    );
                }
                if ds.drawing && !select_mode {
                    let w = Point::new(
                        (position.x - self.st.view.pan_x) / self.st.view.zoom,
                        (position.y - self.st.view.pan_y) / self.st.view.zoom,
                    );
                    return (
                        canvas::event::Status::Captured,
                        Some(Msg::CanvasEvent(CanvasMsg::DragPoint(w))),
                    );
                }
                (canvas::event::Status::Ignored, None)
            }
            Event::Mouse(iced::event::mouse::Event::ButtonReleased(
                iced::event::mouse::Button::Left,
                _,
            )) => {
                if ds.drawing {
                    ds.drawing = false;
                    (
                        canvas::event::Status::Captured,
                        Some(Msg::CanvasEvent(CanvasMsg::DragEnd)),
                    )
                } else {
                    ds.panning = false;
                    (canvas::event::Status::Captured, None)
                }
            }
            Event::Mouse(iced::event::mouse::Event::CursorLeaves) => {
                let was_drawing = ds.drawing;
                ds.drawing = false;
                ds.panning = false;
                if was_drawing {
                    (
                        canvas::event::Status::Captured,
                        Some(Msg::CanvasEvent(CanvasMsg::DragEnd)),
                    )
                } else {
                    (canvas::event::Status::Ignored, None)
                }
            }
            Event::Mouse(iced::event::mouse::Event::Wheel { delta, position, .. }) => {
                let factor = match delta {
                    iced::event::mouse::scroll::Delta::Pixels { y, .. } if y != 0.0 => {
                        if y > 0.0 { 1.08 } else { 1.0 / 1.08 }
                    }
                    iced::event::mouse::scroll::Delta::Lines { y, .. } if y != 0.0 => {
                        if y > 0.0 { 1.15 } else { 1.0 / 1.15 }
                    }
                    _ => return (canvas::event::Status::Ignored, None),
                };
                (
                    canvas::event::Status::Captured,
                    Some(Msg::CanvasEvent(CanvasMsg::ZoomAt(
                        position.x,
                        position.y,
                        factor,
                    ))),
                )
            }
            _ => (canvas::event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        _ds: &DrawState,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        paint(&mut frame, self.st);
        vec![frame.into_geometry()]
    }

    fn mouse_cursor(&self) -> MouseCursor {
        match self.st.tool {
            Tool::Select => MouseCursor::Grab,
            Tool::Text => MouseCursor::Text,
            _ => MouseCursor::Crosshair,
        }
    }
}

fn to_screen(st: &AppState, p: Point) -> (f32, f32) {
    (p.x * st.view.zoom + st.view.pan_x, p.y * st.view.zoom + st.view.pan_y)
}

fn paint(frame: &mut Frame<'_>, st: &AppState) {
    draw_grid(frame, st);

    // Images
    for (_id, pos, hash, w, h) in st.images() {
        let (sx, sy) = to_screen(st, pos);
        let sw = w * st.view.zoom;
        let sh = h * st.view.zoom;
        if let Some(img) = st.image_cache.get(&hash) {
            let handle = state::rgba_image_handle(img.width, img.height, img.pixels.clone());
            frame.draw_image(iced::Rectangle { x: sx, y: sy, width: sw, height: sh }, handle);
        } else {
            fill_rect(frame, sx, sy, sw, sh, iced::Color::from_rgb(0.90, 0.91, 0.94));
            outline_rect(frame, sx, sy, sw, sh, iced::Color::from_rgb(0.6, 0.6, 0.65));
            centered_text(frame, "🖼 image", sx, sy, sw, sh, 13.0);
            let _ = hash;
        }
    }

    // Committed ink
    for s in st.all_strokes() {
        stroke_geometry(
            frame,
            st,
            &s.points,
            color_of(s.color),
            s.width_mm * st.view.zoom,
            leafnote_core::canvas::effective_alpha(&s),
        );
    }
    // Live stroke being drawn right now
    if st.current_stroke.len() > 1 {
        let kind = st.tool.pen_kind().unwrap_or(PenKind::Pen);
        let alpha = if kind == PenKind::Highlighter { 90 } else { 255 };
        stroke_geometry(
            frame,
            st,
            &st.current_stroke,
            color_of(st.color),
            st.width_mm * st.view.zoom,
            alpha,
        );
    }

    // Text outlines rendered on the canvas at their free positions
    for (_bid, pos, width_mm, paras) in st.outlines() {
        let (sx, sy) = to_screen(st, pos);
        let line_h = (12.0 * st.view.zoom * 0.5).clamp(14.0, 30.0);
        let bw = (width_mm * st.view.zoom).max(80.0);
        let bh = line_h * paras.len().max(1) as f32 + 10.0;
        fill_rect(
            frame,
            sx - 2.0,
            sy - 2.0,
            bw + 4.0,
            bh,
            iced::Color::from_rgb(0.995, 0.995, 0.98),
        );
        outline_rect(
            frame,
            sx - 2.0,
            sy - 2.0,
            bw + 4.0,
            bh,
            iced::Color::from_rgb(0.85, 0.85, 0.88),
        );
        for (i, p) in paras.iter().enumerate() {
            let ty = sy + 4.0 + i as f32 * line_h;
            let tx = sx + 6.0 + f32::from(p.level) * 14.0;
            let mut label = String::new();
            if p.tag == Some(NoteTag::ToDo) {
                label.push_str(if p.checked_ { "☑ " } else { "☐ " });
            } else if let Some(t) = &p.tag {
                label.push_str(t.icon());
                label.push(' ');
            }
            label.push_str(&p.text());
            if label.trim().is_empty() {
                label.push('…');
            }
            frame.fill_text(CText {
                content: label,
                position: iced::Point::new(tx, ty),
                color: iced::Color::from_rgb(0.1, 0.1, 0.12),
                size: iced::Pixels(line_h * 0.72),
                line_height: iced::widget::text::LineHeight::default(),
                font: iced::Font::DEFAULT,
                horizontal_alignment: iced::Alignment::Start,
                vertical_alignment: iced::VerticalAlignment::Top,
                shaping: iced::widget::canvas::text::Shaping::Advanced,
            });
        }
    }

    // Attachment chips
    for (_id, pos, name, size) in st.attachments() {
        let (sx, sy) = to_screen(st, pos);
        let chip_w = (name.len() as f32 * 8.0 + 90.0).min(280.0);
        fill_rect(frame, sx, sy, chip_w, 28.0, iced::Color::from_rgb(0.94, 0.95, 0.99));
        outline_rect(frame, sx, sy, chip_w, 28.0, iced::Color::from_rgb(0.55, 0.6, 0.85));
        frame.fill_text(CText {
            content: format!("📎 {name} ({})", human_size(size)),
            position: iced::Point::new(sx + 6.0, sy + 6.0),
            color: iced::Color::from_rgb(0.2, 0.25, 0.5),
            size: iced::Pixels(12.0),
            line_height: iced::widget::text::LineHeight::default(),
            font: iced::Font::DEFAULT,
            horizontal_alignment: iced::Alignment::Start,
            vertical_alignment: iced::VerticalAlignment::Top,
            shaping: iced::widget::canvas::text::Shaping::Basic,
        });
    }
}

fn human_size(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    }
}

fn color_of(c: Color) -> iced::Color {
    iced::Color::from_rgba(
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    )
}

fn stroke_geometry(
    frame: &mut Frame<'_>,
    st: &AppState,
    pts: &[Point],
    color: iced::Color,
    width_px: f32,
    alpha: u8,
) {
    if pts.len() < 2 {
        return;
    }
    let mut path = CPath::new();
    let mut iter = pts.iter();
    let first = iter.next().unwrap();
    let (x0, y0) = to_screen(st, *first);
    path.move_to(iced::Point::new(x0, y0));
    for p in iter {
        let (x, y) = to_screen(st, *p);
        path.line_to(iced::Point::new(x, y));
    }
    let mut color = color;
    color.a = alpha as f32 / 255.0;
    frame.stroke(
        &path,
        CStroke {
            style: canvas::path::Style::Solid(color),
            width: width_px.max(0.7),
            line_cap: canvas::path::LineCap::Round,
            line_join: canvas::path::LineJoin::Round,
            dash: canvas::path::Dash::default(),
            transform: None,
        },
    );
}

fn draw_grid(frame: &mut Frame<'_>, st: &AppState) {
    let step = 10.0 * st.view.zoom; // 10 mm grid
    if step < 6.0 {
        return;
    }
    let dot = iced::Color::from_rgb(0.87, 0.88, 0.91);
    let ox = st.view.pan_x.rem_euclid(step);
    let oy = st.view.pan_y.rem_euclid(step);
    let mut y = oy;
    while y < frame.height() {
        let mut x = ox;
        while x < frame.width() {
            frame.fill(&CPath::circle(iced::Point::new(x, y), 1.1), dot);
            x += step;
        }
        y += step;
    }
}

fn fill_rect(frame: &mut Frame<'_>, x: f32, y: f32, w: f32, h: f32, c: iced::Color) {
    frame.fill(&CPath::rectangle(iced::Point::new(x, y), Size::new(w, h)), c);
}

fn outline_rect(frame: &mut Frame<'_>, x: f32, y: f32, w: f32, h: f32, c: iced::Color) {
    let p = CPath::rectangle(iced::Point::new(x, y), Size::new(w, h));
    frame.stroke(
        &p,
        CStroke {
            style: canvas::path::Style::Solid(c),
            width: 1.0,
            line_cap: canvas::path::LineCap::Butt,
            line_join: canvas::path::LineJoin::Miter,
            dash: canvas::path::Dash::default(),
            transform: None,
        },
    );
}

fn centered_text(frame: &mut Frame<'_>, s: &str, x: f32, y: f32, w: f32, h: f32, size: f32) {
    frame.fill_text(CText {
        content: s.to_string(),
        position: iced::Point::new(x + w / 2.0, y + h / 2.0),
        color: iced::Color::from_rgb(0.4, 0.4, 0.45),
        size: iced::Pixels(size),
        line_height: iced::widget::text::LineHeight::default(),
        font: iced::Font::DEFAULT,
        horizontal_alignment: iced::Alignment::Center,
        vertical_alignment: iced::VerticalAlignment::Center,
        shaping: iced::widget::canvas::text::Shaping::Basic,
    });
}

// ===========================================================================
// Inspector: colours, width, actions, tags, search results
// ===========================================================================

fn inspector_view(st: &AppState) -> Element<'_, Msg> {
    let mut col = Column::new().spacing(8).padding(10);

    col = col.push(text("Tools").size(16));

    let palette: [(u8, u8, u8); 8] = [
        (0, 0, 0),
        (220, 40, 40),
        (240, 150, 20),
        (240, 210, 40),
        (60, 170, 70),
        (40, 110, 220),
        (140, 70, 200),
        (235, 100, 160),
    ];
    let mut swatches = row![].spacing(6).wrap();
    for (r, g, b) in palette {
        let c = Color::rgb(r, g, b);
        let active = st.color.r == r && st.color.g == g && st.color.b == b;
        swatches = swatches.push(
            button(text(if active { "◉" } else { "●" }).size(16))
                .padding([2, 5])
                .style(move |_: &Theme, _: iced::widget::button::Status| {
                    iced::widget::button::Style {
                        background: Some(iced::Background::Color(iced::Color::from_rgb(
                            r as f32 / 255.0,
                            g as f32 / 255.0,
                            b as f32 / 255.0,
                        ))),
                        text_color: Some(iced::Color::WHITE),
                        border: iced::Border {
                            radius: 6.0.into(),
                            width: if active { 2.0 } else { 0.0 },
                            color: iced::Color::BLACK,
                        },
                        shadow: Default::default(),
                    }
                })
                .on_press(Msg::SetColor(c)),
        );
    }
    col = col.push(swatches);

    col = col.push(text(format!("Stroke width: {:.1} mm", st.width_mm)).size(12));
    col = col.push(
        iced::widget::slider(0.2..=12.0, st.width_mm, Msg::SetWidth)
            .step(0.1)
            .height(24),
    );

    col = col.push(
        row![
            button(text("↶ Undo").size(12)).padding([4, 8]).on_press(Msg::Undo),
            button(text("🖼 Insert image").size(12))
                .padding([4, 8])
                .on_press(Msg::RequestPick(PickTarget::InsertImage)),
            button(text("📎 Attach file").size(12))
                .padding([4, 8])
                .on_press(Msg::RequestPick(PickTarget::InsertFile)),
        ]
        .spacing(6)
        .wrap(),
    );

    if st.is_read_only() {
        col = col.push(
            button(text("🔓 Upgrade section to editable").size(12))
                .padding([6, 8])
                .width(Length::Fill)
                .on_press(Msg::UpgradeSection),
        );
    }

    // Outline editor: live paragraph rows for every text block on the page
    if let Some(page) = st.page() {
        let mut outs = Column::new().spacing(2);
        let mut any = false;
        for b in &page.blocks {
            if let Block::Outline { paragraphs, .. } = b {
                for (i, p) in paragraphs.iter().enumerate() {
                    any = true;
                    let idx = i;
                    let mut line = row![].spacing(4);
                    if p.tag == Some(NoteTag::ToDo) {
                        line = line.push(button(
                            text(if p.checked_ { "☑" } else { "☐" }).size(14))
                            .padding([2, 4])
                            .on_press(Msg::CanvasEvent(CanvasMsg::ToggleCheck(idx))),
                        );
                    }
                    line = line.push(
                        text_input("Write a note…", &p.text())
                            .on_input(move |t| Msg::CanvasEvent(CanvasMsg::ParaEdited(idx, t)))
                            .padding(5)
                            .size(13),
                    );
                    outs = outs.push(line);
                }
            }
        }
        if any {
            col = col.push(text("Page text").size(14));
            col = col.push(scrollable(outs).height(Length::FillPortion(2)));
        }
    }

    // search
    col = col.push(vertical_space().height(4));
    col = col.push(text("Search all notebooks").size(16));
    col = col.push(
        text_input("Find titles, text, tags…", &st.query)
            .on_input(Msg::Query)
            .on_submit(Msg::RunSearch)
            .padding(6)
            .size(13),
    );
    let hits_col: Column<Msg> = if st.hits.is_empty() && !st.query.trim().is_empty() {
        column![text("No matches.").size(12)]
    } else if st.hits.is_empty() {
        column![text("Type to search across every open notebook.").size(12)]
    } else {
        let mut c = Column::new().spacing(4);
        for h in st.hits.iter().take(20) {
            c = c.push(search_hit_row(h));
        }
        c
    };
    col = col.push(scrollable(hits_col).height(Length::FillPortion(3)));

    container(col).height(Length::Fill).into()
}

fn search_hit_row(h: &SearchHit) -> Element<'static, Msg> {
    let path = if h.group_path.is_empty() {
        h.section_name.clone()
    } else {
        format!("{} › {}", h.group_path.join(" › "), h.section_name)
    };
    let hit = h.clone();
    button(
        column![
            text(h.title.clone()).size(13),
            text(path).size(11).color(iced::Color::from_rgb(0.4, 0.45, 0.6)),
            text(h.snippet.clone()).size(11).color(iced::Color::from_rgb(0.35, 0.35, 0.4)),
        ]
        .spacing(2)
        .padding([4, 6]),
    )
    .width(Length::Fill)
    .style(move |th: &Theme, status| theme::Button::secondary(th, status))
    .on_press(Msg::OpenHit(hit))
    .into()
}

// ===========================================================================
// Status bar
// ===========================================================================

fn status_bar(st: &AppState) -> Element<'_, Msg> {
    let zoom = format!("{:.0}%", st.view.zoom * 100.0 / 3.6);
    container(
        row![
            text(st.status.clone()).size(12),
            horizontal_space(),
            text(zoom).size(12),
            Space::with_width(10),
            text("local-first · no account · no cloud")
                .size(11)
                .color(iced::Color::from_rgb(0.5, 0.55, 0.6)),
        ]
        .padding([6, 12])
        .align_y(iced::Align::Center),
    )
    .width(Length::Fill)
    .style(|_: &Theme| container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb(0.93, 0.94, 0.96))),
        ..Default::default()
    })
    .into()
}
