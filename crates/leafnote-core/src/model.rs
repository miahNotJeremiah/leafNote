//! The leafNote document model.
//!
//! Mirrors OneNote's hierarchy: **Notebook → (Section Groups →) Sections → Pages**,
//! where each page is a *freeform canvas* containing absolutely-positioned
//! content blocks: rich-text outlines, ink drawings, images, and file
//! attachments — exactly like an OneNote 2016 page.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Millimetres per inch — OneNote stores offsets in half-inches; we normalise
/// to millimetres internally so the canvas is DPI-independent.
pub const MM_PER_INCH: f32 = 25.4;
/// OneNote layout units are half-inch increments.
pub const HALF_INCH: f32 = 0.5 * MM_PER_INCH;

/// Convert OneNote half-inch offsets to leafNote millimetres.
pub fn half_inch_to_mm(v: f32) -> f32 {
    v * HALF_INCH
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Id(#[serde(with = "uuid_str")] pub Uuid);

/// Serde helper storing UUIDs as plain strings for human-readable JSON files.
mod uuid_str {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    pub fn serialize<S: Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&id.simple().to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
        let raw = String::deserialize(d)?;
        Uuid::try_parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl Id {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }
}
impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}

/// RGBA colour, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    /// Parse `#RRGGBB` / `#RRGGBBAA`.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        let bytes = hex::decode_bytes(s)?;
        match bytes.as_slice() {
            [r, g, b] => Some(Self::rgb(*r, *g, *b)),
            [r, g, b, a] => Some(Self {
                r: *r,
                g: *g,
                b: *b,
                a: *a,
            }),
            _ => None,
        }
    }
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const RED: Color = Color::rgb(0xC0, 0x39, 0x2B);
    pub const BLUE: Color = Color::rgb(0x1A, 0x7F, 0xFF);
    pub const GREEN: Color = Color::rgb(0x2E, 0xCC, 0x71);
    pub const YELLOW_HIGHLIGHT: Color = Color::rgb(0xFF, 0xF3, 0xA0);
}

mod hex {
    pub(super) fn decode_bytes(s: &str) -> Option<Vec<u8>> {
        if s.is_empty() || s.len() % 2 != 0 {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    }
}

/// A point on the freeform canvas, in millimetres from the page origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn distance(self, o: Point) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

/// An axis-aligned rectangle on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }
    pub fn union(&self, o: Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let right = (self.x + self.w).max(o.x + o.w);
        let bottom = (self.y + self.h).max(o.y + o.h);
        Rect::new(x, y, right - x, bottom - y)
    }
}

// ---------------------------------------------------------------------------
// Rich text
// ---------------------------------------------------------------------------

/// Paragraph alignment, mirroring MS-ONE `ParagraphAlignment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

/// Character/paragraph formatting for one run of text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font: String,
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub color: Color,
    pub highlight: Option<Color>,
    pub hyperlink: Option<String>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: "Calibri".into(),
            size_pt: 11.0,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            color: Color::BLACK,
            highlight: None,
            hyperlink: None,
        }
    }
}

/// A contiguous span of text sharing one [`TextStyle`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    pub text: String,
    pub style: TextStyle,
}

impl TextRun {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: TextStyle::default(),
        }
    }
}

/// Built-in note tags, matching the familiar OneNote set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteTag {
    ToDo,
    Important,
    Question,
    Contact,
    Address,
    Priority1,
    Priority2,
    Priority3,
    Idea,
    Password,
    TrackingCritical,
    TrackingFinished,
}

impl NoteTag {
    pub fn label(&self) -> &'static str {
        match self {
            NoteTag::ToDo => "To Do",
            NoteTag::Important => "Important",
            NoteTag::Question => "Question",
            NoteTag::Contact => "Contact",
            NoteTag::Address => "Address",
            NoteTag::Priority1 => "Priority 1",
            NoteTag::Priority2 => "Priority 2",
            NoteTag::Priority3 => "Priority 3",
            NoteTag::Idea => "Idea",
            NoteTag::Password => "Password",
            NoteTag::TrackingCritical => "Critical",
            NoteTag::TrackingFinished => "Finished",
        }
    }
    pub fn icon(&self) -> &'static str {
        match self {
            NoteTag::ToDo => "☐",
            NoteTag::Important => "❗",
            NoteTag::Question => "❓",
            NoteTag::Contact => "👤",
            NoteTag::Address => "📍",
            NoteTag::Priority1 | NoteTag::Priority2 | NoteTag::Priority3 => "⭐",
            NoteTag::Idea => "💡",
            NoteTag::Password => "🔑",
            NoteTag::TrackingCritical => "🔴",
            NoteTag::TrackingFinished => "✅",
        }
    }
    pub fn all() -> &'static [NoteTag] {
        &[
            NoteTag::ToDo,
            NoteTag::Important,
            NoteTag::Question,
            NoteTag::Contact,
            NoteTag::Address,
            NoteTag::Priority1,
            NoteTag::Priority2,
            NoteTag::Priority3,
            NoteTag::Idea,
            NoteTag::Password,
            NoteTag::TrackingCritical,
            NoteTag::TrackingFinished,
        ]
    }
}

/// One paragraph inside an outline block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paragraph {
    /// Indentation level (0 = top level), like OneNote outline levels.
    pub level: u8,
    pub alignment: Alignment,
    pub runs: Vec<TextRun>,
    pub tag: Option<NoteTag>,
    /// For [`NoteTag::ToDo`]: whether the checkbox is ticked.
    pub checked_: bool,
}

impl Paragraph {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            level: 0,
            alignment: Alignment::Left,
            runs: vec![TextRun::plain(text)],
            tag: None,
            checked_: false,
        }
    }
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// A simple table (OneNote pages can contain tables too).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TableBlock {
    pub rows: Vec<Vec<Paragraph>>,
    pub col_widths_mm: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Ink / media
// ---------------------------------------------------------------------------

/// Pen tools available on the drawing canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PenKind {
    /// Ballpoint-like thin pen.
    Pen,
    /// Marker/highlighter — translucent, wide.
    Highlighter,
    /// Pencil.
    Pencil,
    /// Lasso/eraser stroke.
    Eraser,
}

/// One freehand stroke captured from mouse/pen input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub points: Vec<Point>,
    pub color: Color,
    /// Width in millimetres.
    pub width_mm: f32,
    pub kind: PenKind,
}

impl Stroke {
    pub fn bounding_box(&self) -> Rect {
        crate::canvas::stroke_bounding_box(self)
    }
}

/// Reference to image bytes stored content-addressed under `media/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageBlock {
    /// SHA-256 (hex) of the media blob in the notebook's `media/` folder.
    pub media_hash: String,
    pub file_name: String,
    pub mime: String,
    pub width_mm: f32,
    pub height_mm: f32,
    pub alt_text: Option<String>,
}

/// Reference to an embedded file attachment stored under `media/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub media_hash: String,
    pub file_name: String,
    pub mime: String,
    pub size_bytes: u64,
}

/// Anything that can be placed freely on the page canvas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Block {
    Outline {
        id: Id,
        pos: Point,
        width_mm: f32,
        paragraphs: Vec<Paragraph>,
    },
    Ink {
        id: Id,
        strokes: Vec<Stroke>,
    },
    Image {
        id: Id,
        pos: Point,
        image: ImageBlock,
    },
    File {
        id: Id,
        pos: Point,
        attachment: Attachment,
    },
    Table {
        id: Id,
        pos: Point,
        table: TableBlock,
    },
}

impl Block {
    pub fn id(&self) -> Id {
        match self {
            Block::Outline { id, .. }
            | Block::Ink { id, .. }
            | Block::Image { id, .. }
            | Block::File { id, .. }
            | Block::Table { id, .. } => *id,
        }
    }
    /// Plain text contributed by this block (for search & export).
    pub fn plain_text(&self) -> String {
        match self {
            Block::Outline { paragraphs, .. } => {
                paragraphs.iter().map(|p| p.text()).collect::<Vec<_>>().join("\n")
            }
            Block::Ink { strokes, .. } => strokes
                .iter()
                .filter_map(|_| None::<String>)
                .collect::<Vec<_>>()
                .join(""),
            Block::Image { image, .. } => image.alt_text.clone().unwrap_or_default(),
            Block::File { attachment, .. } => attachment.file_name.clone(),
            Block::Table { table, .. } => table
                .rows
                .iter()
                .flat_map(|r| r.iter())
                .map(|c| c.text())
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }
}

// ---------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------

/// A single note page — the freeform canvas.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub id: Id,
    pub title: String,
    /// Sub-pages supported like OneNote (`level` > 0 means nested under previous page).
    pub level: u8,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub author: Option<String>,
    /// Freeform canvas contents.
    pub blocks: Vec<Block>,
    /// Optional locked/fixed page size; `None` = infinite canvas.
    pub size_mm: Option<(f32, f32)>,
    /// Set when this page was imported read-only from a `.one` file.
    pub origin: Option<String>,
}

impl Page {
    pub fn new(title: impl Into<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Id::new(),
            title: title.into(),
            level: 0,
            created: now,
            modified: now,
            author: None,
            blocks: Vec::new(),
            size_mm: None,
            origin: None,
        }
    }

    /// Add a default empty text block at the given canvas position.
    pub fn add_text_block(&mut self, pos: Point) -> Id {
        let id = Id::new();
        self.blocks.push(Block::Outline {
            id,
            pos,
            width_mm: 120.0,
            paragraphs: vec![Paragraph::plain("")],
        });
        self.touch();
        id
    }

    pub fn touch(&mut self) {
        self.modified = chrono::Utc::now();
    }

    /// All tags used on the page.
    pub fn tags(&self) -> Vec<NoteTag> {
        let mut out = Vec::new();
        for b in &self.blocks {
            if let Block::Outline { paragraphs, .. } = b {
                for p in paragraphs {
                    if let Some(t) = &p.tag {
                        if !out.contains(t) {
                            out.push(*t);
                        }
                    }
                }
            }
        }
        out
    }

    /// Full plain text of the page (title + body), used by the search index.
    pub fn full_text(&self) -> String {
        let mut s = String::with_capacity(256);
        s.push_str(&self.title);
        s.push('\n');
        for b in &self.blocks {
            s.push_str(&b.plain_text());
            s.push('\n');
        }
        s
    }
}

/// A section — corresponds to a `.one` file in OneNote.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Section {
    pub id: Id,
    pub name: String,
    pub color: Option<Color>,
    /// Order of pages on disk = order in `page_ids`.
    pub page_ids: Vec<Id>,
    #[serde(skip)]
    /// Loaded lazily by the [`crate::store::Store`].
    pub pages: Vec<Page>,
    /// Set when imported read-only from OneNote.
    pub read_only: bool,
    pub origin: Option<String>,
}

impl Section {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            color: None,
            page_ids: Vec::new(),
            pages: Vec::new(),
            read_only: false,
            origin: None,
        }
    }
}

/// Tree node kinds inside a notebook.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TreeNode {
    Section(SectionRef),
    Group(GroupRef),
}

/// Lightweight reference — heavy content lives in separate files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionRef {
    pub id: Id,
    pub name: String,
    pub color: Option<Color>,
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupRef {
    pub id: Id,
    pub name: String,
    pub children: Vec<TreeNode>,
}

/// A notebook — corresponds to a `.onetoc2` + folder in OneNote.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notebook {
    pub id: Id,
    pub name: String,
    pub color: Option<Color>,
    pub root: Vec<TreeNode>,
    /// Directory on disk backing this notebook.
    #[serde(skip)]
    pub path: std::path::PathBuf,
    /// Whole notebook opened in read-only mode (e.g. imported backup).
    pub read_only: bool,
}

impl Notebook {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            color: None,
            root: Vec::new(),
            path: Default::default(),
            read_only: false,
        }
    }

    /// Flatten the tree into (group-path, section-ref) pairs for the sidebar.
    pub fn sections_flat(&self) -> Vec<(Vec<String>, SectionRef)> {
        fn walk(
            nodes: &[TreeNode],
            path: &mut Vec<String>,
            out: &mut Vec<(Vec<String>, SectionRef)>,
        ) {
            for n in nodes {
                match n {
                    TreeNode::Section(s) => out.push((path.clone(), s.clone())),
                    TreeNode::Group(g) => {
                        path.push(g.name.clone());
                        walk(&g.children, path, out);
                        path.pop();
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_hex_roundtrip() {
        let c = Color::from_hex("#1a7fff").unwrap();
        assert_eq!(c, Color::BLUE);
        assert_eq!(c.to_hex(), "#1a7fff");
        assert!(Color::from_hex("nope").is_none());
    }

    #[test]
    fn rect_union_contains_both() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 20.0, 10.0, 10.0);
        let u = a.union(b);
        assert!(u.contains(Point::new(1.0, 1.0)) && u.contains(Point::new(14.0, 29.0)));
    }

    #[test]
    fn page_full_text_includes_title_and_body() {
        let mut p = Page::new("Groceries");
        let id = p.add_text_block(Point::new(0.0, 0.0));
        if let Some(Block::Outline { paragraphs, .. }) =
            p.blocks.iter_mut().find(|b| b.id() == id)
        {
            paragraphs[0] = Paragraph::plain("Milk, eggs");
        }
        assert!(p.full_text().contains("Groceries"));
        assert!(p.full_text().contains("Milk, eggs"));
    }
}
