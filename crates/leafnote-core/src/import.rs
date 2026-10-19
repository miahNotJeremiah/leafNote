//! Native **read-only** import of real Microsoft OneNote files, built on the
//! [`onenote_parser`] crate.
//!
//! Supported inputs (auto-detected by extension & content):
//! - `.one`      — a single section file (pages, rich text, ink, images, attachments)
//! - `.onetoc2`  — a notebook table of contents (full hierarchy; parses every section)
//! - `.onepkg`   — a CAB package archive bundling a `.onetoc2` + its `.one` files
//!
//! Import never writes to the source files: the originals stay pristine
//! backups. Everything is converted into leafNote's own model and saved into a
//! *new* notebook folder in read-only mode (authoring happens after you
//! "upgrade" a page, or simply by creating native pages).

use std::io::Read;
use std::path::{Path, PathBuf};

use onenote_parser::contents::{Content, OutlineItem};
use onenote_parser::page::{Page as OnePage, PageContent};
use onenote_parser::property::rich_text::ParagraphAlignment;
use onenote_parser::section::{Section as OneSection, SectionEntry};
use onenote_parser::Parser;
use typed_path::TypedPath;

use crate::error::{CoreError, Result};
use crate::model::{
    half_inch_to_mm, Alignment, Attachment, Block, Color, GroupRef, Id, ImageBlock, Notebook,
    NoteTag, Page, Paragraph, PenKind, Point, Rect, Section, SectionRef, Stroke, TableBlock,
    TextStyle, TextRun, TreeNode,
};
use crate::store::{sanitize_folder_name, Store};

/// What kind of OneNote artifact a path points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneFileKind {
    /// A `.one` section file.
    Section,
    /// A `.onetoc2` notebook table of contents.
    Toc,
    /// A `.onepkg` package archive.
    Package,
}

impl OneFileKind {
    pub fn detect(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("one") => Some(Self::Section),
            Some("onetoc2") => Some(Self::Toc),
            Some("onepkg") => Some(Self::Package),
            _ => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Section => "OneNote section (.one)",
            Self::Toc => "OneNote notebook (.onetoc2)",
            Self::Package => "OneNote package (.onepkg)",
        }
    }
}

/// Summary returned after an import completes.
#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub notebook: Option<Id>,
    pub sections: usize,
    pub pages: usize,
    pub ink_strokes: usize,
    pub images: usize,
    pub attachments: usize,
    /// Non-fatal warnings emitted by the parser (unreadable objects etc.).
    pub warnings: Vec<String>,
}

/// Convert a `ColorRef` from the parser into our model colour.
fn conv_color_ref(c: onenote_parser::property::common::ColorRef) -> Color {
    use onenote_parser::property::common::ColorRef;
    match c {
        ColorRef::Manual { r, g, b } => Color::rgb(r, g, b),
        ColorRef::Auto => Color::BLACK,
    }
}

fn conv_opt_color(c: Option<onenote_parser::property::common::Color>) -> Option<Color> {
    c.map(|c| Color::rgb(c.r(), c.g(), c.b()))
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Open any supported OneNote file and convert it into a fresh leafNote
/// notebook stored under `data_root/<Name> (imported)/`.
///
/// The source file is only ever *read*.
pub fn import_any(
    store: &mut Store,
    data_root: &Path,
    path: &Path,
) -> Result<(ImportReport, PathBuf)> {
    let kind = OneFileKind::detect(path)
        .ok_or_else(|| CoreError::Other(format!("not a OneNote file: {}", path.display())))?;
    let base = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Imported Notebook".into());
    // ".foo.one" stems to ".foo"; strip trailing OneNote noise.
    let name = base.trim_end_matches(".one").trim().to_string();
    let nb_dir_name = format!("{} (imported)", sanitize_folder_name(&name));
    let target = unique_import_dir(data_root, &nb_dir_name);
    std::fs::create_dir_all(&target)?;
    let mut nb = Notebook::new(name);
    nb.path = target.clone();
    nb.read_only = true;
    crate::store::write_json(
        &target.join("tree.json"),
        &crate::store::NotebookMetaFile {
            version: crate::FORMAT_VERSION,
            notebook: nb.clone(),
        },
    )?;
    let nb_id = nb.id;
    store.open_notebook(&target)?;

    let mut report = match kind {
        OneFileKind::Section => import_section_file(store, nb_id, path)?,
        OneFileKind::Toc | OneFileKind::Package => {
            import_notebook_file(store, nb_id, path, kind)?
        }
    };
    report.notebook = Some(nb_id);
    Ok((report, target))
}

/// Never clobber a previous import of the same notebook: pick
/// `Name (imported)`, `Name (imported 2)`, …
fn unique_import_dir(data_root: &Path, base: &str) -> PathBuf {
    let first = data_root.join(base);
    if !first.exists() {
        return first;
    }
    for i in 2..1000 {
        let p = data_root.join(format!("{base} {i}"));
        if !p.exists() {
            return p;
        }
    }
    // Astronomically unlikely fallback: timestamped suffix.
    data_root.join(format!(
        "{base} {}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ))
}

fn import_section_file(store: &mut Store, nb_id: Id, path: &Path) -> Result<ImportReport> {
    let parser = Parser::new();
    let tp = typed_path_from(path);
    let section = parser
        .parse_section(tp)
        .map_err(|e| CoreError::OneNote(e.to_string()))?;
    let dir = store
        .notebook_dir(nb_id)
        .ok_or_else(|| CoreError::NotFound(format!("notebook {nb_id}")))?;
    let mut report = ImportReport::default();
    report.warnings.extend(
        section
            .report()
            .warnings()
            .iter()
            .map(|w| w.message().to_string()),
    );
    let sec = convert_section(store, &dir, &section, path, &mut report)?;
    store.register_section(nb_id, sec)?;
    report.sections = 1;
    Ok(report)
}

fn import_notebook_file(
    store: &mut Store,
    nb_id: Id,
    path: &Path,
    kind: OneFileKind,
) -> Result<ImportReport> {
    let parser = Parser::new();
    let tp = typed_path_from(path);
    let notebook = match kind {
        OneFileKind::Package => parser
            .parse_package(tp)
            .map_err(|e| CoreError::OneNote(format!(".onepkg: {e}")))?,
        _ => parser
            .parse_notebook(tp)
            .map_err(|e| CoreError::OneNote(format!(".onetoc2: {e}")))?,
    };
    let dir = store
        .notebook_dir(nb_id)
        .ok_or_else(|| CoreError::NotFound(format!("notebook {nb_id}")))?;
    let mut report = ImportReport::default();
    report.warnings.extend(
        notebook
            .report()
            .warnings()
            .iter()
            .map(|w| w.message().to_string()),
    );
    if let Some(c) = notebook.color() {
        if let Some(nb) = store.notebook_mut(nb_id) {
            nb.color = conv_opt_color(Some(c));
        }
    }
    let nodes = convert_entries(store, nb_id, &dir, notebook.entries(), path, &mut report)?;
    report.sections = count_sections(&nodes);
    if let Some(nb) = store.notebook_mut(nb_id) {
        nb.root = nodes;
        crate::store::write_json(
            &nb.path.join("tree.json"),
            &crate::store::NotebookMetaFile {
                version: crate::FORMAT_VERSION,
                notebook: nb.clone(),
            },
        )?;
    }
    Ok(report)
}

fn count_sections(nodes: &[TreeNode]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            TreeNode::Section(_) => 1,
            TreeNode::Group(g) => count_sections(&g.children),
        })
        .sum()
}

fn convert_entries(
    store: &mut Store,
    nb_id: Id,
    dir: &Path,
    entries: &[SectionEntry],
    src: &Path,
    report: &mut ImportReport,
) -> Result<Vec<TreeNode>> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            SectionEntry::Section(s) => {
                let sec = convert_section(store, dir, s, src, report)?;
                let reg = store.register_section(nb_id, sec)?;
                out.push(TreeNode::Section(SectionRef {
                    id: reg.id,
                    name: reg.name.clone(),
                    color: reg.color,
                    read_only: true,
                }));
            }
            SectionEntry::SectionGroup(g) => {
                let children = convert_entries(store, nb_id, dir, g.entries(), src, report)?;
                out.push(TreeNode::Group(GroupRef {
                    id: Id::new(),
                    name: g.display_name().to_string(),
                    children,
                }));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Section / page conversion
// ---------------------------------------------------------------------------

fn convert_section(
    store: &mut Store,
    dir: &Path,
    one: &OneSection,
    src: &Path,
    report: &mut ImportReport,
) -> Result<Section> {
    let mut sec = Section::new(one.display_name());
    sec.color = one.color().map(|c| Color::rgb(c.r(), c.g(), c.b()));
    sec.read_only = true;
    sec.origin = Some(src.display().to_string());
    std::fs::create_dir_all(dir.join("sections"))?;
    std::fs::create_dir_all(dir.join("pages"))?;

    for series in one.page_series() {
        for p in series.pages() {
            let page = convert_page(store, dir, p, src, report)?;
            let pid = page.id;
            crate::store::write_json(&dir.join("pages").join(format!("{pid}.json")), &page)?;
            sec.page_ids.push(pid);
            report.pages += 1;
        }
    }
    Ok(sec)
}

fn convert_page(
    store: &mut Store,
    dir: &Path,
    one: &OnePage,
    src: &Path,
    report: &mut ImportReport,
) -> Result<Page> {
    let title = one
        .title_text()
        .map(|t| t.replace('\n', " ").trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled page".into());

    let created = utc_from(one.created_time());
    let modified = utc_from(one.updated_time());

    let mut page = Page {
        id: Id::new(),
        title,
        level: one.level().max(0) as u8,
        created,
        modified,
        author: one.author().map(|a| a.to_string()),
        blocks: Vec::new(),
        size_mm: one.height().map(|h| (210.0, half_inch_to_mm(h).max(40.0))),
        origin: Some(src.display().to_string()),
    };

    // The page title itself becomes a fixed outline block at its canvas spot.
    if let Some(t) = one.title() {
        let pos = Point::new(
            half_inch_to_mm(t.offset_horizontal()),
            half_inch_to_mm(t.offset_vertical()),
        );
        let mut paragraphs = Vec::new();
        for outline in t.contents() {
            collect_outline_paragraphs(outline.items(), &mut paragraphs);
        }
        if paragraphs.is_empty() {
            paragraphs.push(Paragraph::plain(page.title.clone()));
        }
        page.blocks.push(Block::Outline {
            id: Id::new(),
            pos,
            width_mm: 150.0,
            paragraphs,
        });
    }

    for pc in one.contents() {
        match pc {
            PageContent::Outline(o) => {
                let mut paragraphs = Vec::new();
                collect_outline_paragraphs(o.items(), &mut paragraphs);
                let pos = Point::new(
                    half_inch_to_mm(o.offset_horizontal().unwrap_or(0.0)),
                    half_inch_to_mm(o.offset_vertical().unwrap_or(0.0)),
                );
                page.blocks.push(Block::Outline {
                    id: Id::new(),
                    pos,
                    width_mm: o.layout_max_width().map(half_inch_to_mm).unwrap_or(160.0),
                    paragraphs,
                });
            }
            PageContent::Image(img) => {
                if let Some(block) = convert_image(store, dir, img) {
                    report.images += 1;
                    page.blocks.push(block);
                }
            }
            PageContent::EmbeddedFile(f) => {
                if let Some(block) = convert_attachment(store, dir, f) {
                    report.attachments += 1;
                    page.blocks.push(block);
                }
            }
            PageContent::Ink(ink) => {
                let pos = Point::new(
                    ink.offset_horizontal().map(half_inch_to_mm).unwrap_or(0.0),
                    ink.offset_vertical().map(half_inch_to_mm).unwrap_or(0.0),
                );
                let strokes = convert_ink(ink, pos, report);
                if !strokes.is_empty() {
                    page.blocks.push(Block::Ink {
                        id: Id::new(),
                        strokes,
                    });
                }
            }
            PageContent::Unknown => {}
        }
    }

    Ok(page)
}

fn collect_outline_paragraphs(items: &[OutlineItem], out: &mut Vec<Paragraph>) {
    for item in items {
        match item {
            OutlineItem::Element(el) => {
                for c in el.contents() {
                    match c {
                        Content::RichText(rt) => out.push(convert_rich_text(rt, el.child_level())),
                        Content::Table(tbl) => {
                            // Flatten tables into "a | b | c" paragraph rows so
                            // their text stays searchable.
                            for row in tbl.contents() {
                                let mut cells = Vec::new();
                                for cell in row.contents() {
                                    let txt: Vec<String> = cell
                                        .contents()
                                        .iter()
                                        .flat_map(|el2| {
                                            el2.contents().iter().filter_map(|c2| {
                                                c2.rich_text().map(|rt| rt.text().to_string())
                                            })
                                        })
                                        .collect();
                                    cells.push(txt.join(" "));
                                }
                                out.push(Paragraph::plain(cells.join(" | ")));
                            }
                        }
                        _ => {}
                    }
                }
                collect_outline_paragraphs(el.children(), out);
            }
            OutlineItem::Group(g) => collect_outline_paragraphs(g.outlines(), out),
        }
    }
}

/// Full-fidelity table conversion (kept for the editor's table support; the
/// paragraph flattening above is what search/snippets currently use).
#[allow(dead_code)]
fn convert_table(tbl: &onenote_parser::contents::Table) -> TableBlock {
    TableBlock {
        rows: tbl
            .contents()
            .iter()
            .map(|row| {
                row.contents()
                    .iter()
                    .map(|cell| {
                        let txt: Vec<String> = cell
                            .contents()
                            .iter()
                            .flat_map(|el| {
                                el.contents()
                                    .iter()
                                    .filter_map(|c| c.rich_text().map(|rt| rt.text().to_string()))
                            })
                            .collect();
                        Paragraph::plain(txt.join(" "))
                    })
                    .collect()
            })
            .collect(),
        col_widths_mm: tbl.col_widths().iter().map(|w| half_inch_to_mm(*w)).collect(),
    }
}

fn convert_rich_text(rt: &onenote_parser::contents::RichText, level: u8) -> Paragraph {
    let text = rt.text();
    let indices = rt.text_run_indices();
    let styles = rt.text_run_formatting();
    let mut runs = Vec::new();
    if indices.is_empty() || styles.is_empty() {
        runs.push(TextRun {
            text: text.to_string(),
            style: para_style(rt.paragraph_style()),
        });
    } else {
        let chars: Vec<char> = text.chars().collect();
        for (i, window) in indices.windows(2).enumerate() {
            let (a, b) = (window[0] as usize, window[1] as usize);
            let style = styles
                .get(i)
                .or(styles.last())
                .map(para_style)
                .unwrap_or_default();
            let chunk: String = chars[a.min(chars.len())..b.min(chars.len())].iter().collect();
            runs.push(TextRun { text: chunk, style });
        }
        if let Some(&last) = indices.last() {
            let rest: String = chars[(last as usize).min(chars.len())..].iter().collect();
            if !rest.is_empty() {
                let style = styles.last().map(para_style).unwrap_or_default();
                runs.push(TextRun { text: rest, style });
            }
        }
    }

    let alignment = match rt.paragraph_alignment() {
        ParagraphAlignment::Left => Alignment::Left,
        ParagraphAlignment::Center => Alignment::Center,
        ParagraphAlignment::Right => Alignment::Right,
        _ => Alignment::Left,
    };

    let tag = rt
        .note_tags()
        .first()
        .and_then(|t| t.definition())
        .map(|d| map_tag(d.label()));

    Paragraph {
        level,
        alignment,
        runs,
        tag,
        checked_: false,
    }
}

fn para_style(p: &onenote_parser::contents::ParagraphStyling) -> TextStyle {
    TextStyle {
        font: p.font().unwrap_or("Calibri").to_string(),
        size_pt: p.font_size().unwrap_or(11) as f32,
        bold: p.bold(),
        italic: p.italic(),
        underline: p.underline(),
        strikethrough: p.strikethrough(),
        color: p.font_color().map(conv_color_ref).unwrap_or(Color::BLACK),
        highlight: p.highlight().map(conv_color_ref),
        hyperlink: None,
    }
}

fn map_tag(label: &str) -> NoteTag {
    let l = label.to_ascii_lowercase();
    if l.contains("important") {
        NoteTag::Important
    } else if l.contains("question") {
        NoteTag::Question
    } else if l.contains("contact") {
        NoteTag::Contact
    } else if l.contains("address") {
        NoteTag::Address
    } else if l.contains("priority 1") {
        NoteTag::Priority1
    } else if l.contains("priority 2") {
        NoteTag::Priority2
    } else if l.contains("priority 3") {
        NoteTag::Priority3
    } else if l.contains("idea") {
        NoteTag::Idea
    } else if l.contains("password") {
        NoteTag::Password
    } else if l.contains("critical") {
        NoteTag::TrackingCritical
    } else if l.contains("finish") || l.contains("done") {
        NoteTag::TrackingFinished
    } else {
        NoteTag::ToDo
    }
}

fn convert_image(
    _store: &Store,
    dir: &Path,
    img: &onenote_parser::contents::Image,
) -> Option<Block> {
    let mut buf = Vec::new();
    if let Some(mut r) = img.read() {
        if r.read_to_end(&mut buf).is_err() || buf.is_empty() {
            return None;
        }
    } else {
        return None;
    }
    let hash = Store::put_media(dir, &buf).ok()?;
    let ext = img.extension().unwrap_or("png").to_string();
    let pos = Point::new(
        img.offset_horizontal().map(half_inch_to_mm).unwrap_or(0.0),
        img.offset_vertical().map(half_inch_to_mm).unwrap_or(0.0),
    );
    let w = img.picture_width().map(half_inch_to_mm).unwrap_or(80.0);
    let h = img.picture_height().map(half_inch_to_mm).unwrap_or(60.0);
    Some(Block::Image {
        id: Id::new(),
        pos,
        image: ImageBlock {
            media_hash: hash,
            file_name: img.image_filename().unwrap_or("image").to_string(),
            mime: format!("image/{ext}"),
            width_mm: w,
            height_mm: h,
            alt_text: img.alt_text().map(|s| s.to_string()),
        },
    })
}

fn convert_attachment(
    _store: &Store,
    dir: &Path,
    f: &onenote_parser::contents::EmbeddedFile,
) -> Option<Block> {
    let mut buf = Vec::new();
    let mut r = f.read();
    if r.read_to_end(&mut buf).is_err() || buf.is_empty() {
        return None;
    }
    let hash = Store::put_media(dir, &buf).ok()?;
    let pos = Point::new(
        f.offset_horizontal().map(half_inch_to_mm).unwrap_or(0.0),
        f.offset_vertical().map(half_inch_to_mm).unwrap_or(0.0),
    );
    Some(Block::File {
        id: Id::new(),
        pos,
        attachment: Attachment {
            media_hash: hash,
            // SECURITY: filenames coming from foreign files are sanitised so a
            // malicious .one cannot smuggle "../evil" into our export paths.
            file_name: sanitize_folder_name(f.filename()),
            mime: format!("{:?}", f.file_type()).to_lowercase(),
            size_bytes: f.size(),
        },
    })
}

fn convert_ink(
    ink: &onenote_parser::contents::Ink,
    offset: Point,
    report: &mut ImportReport,
) -> Vec<Stroke> {
    let mut out = Vec::new();
    collect_ink(ink, offset, &mut out);
    report.ink_strokes += out.len();
    out
}

fn collect_ink(ink: &onenote_parser::contents::Ink, offset: Point, out: &mut Vec<Stroke>) {
    for s in ink.ink_strokes() {
        let pts: Vec<Point> = s
            .path()
            .iter()
            .map(|p| Point::new(offset.x + p.x(), offset.y + p.y()))
            .collect();
        let color = match s.color() {
            Some(v) => {
                // MS-ONE stores stroke colour as 0x00RRGGBB-ish u32.
                Color::rgb(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
            }
            None => Color::BLACK,
        };
        let alpha = s.transparency().unwrap_or(255);
        out.push(Stroke {
            points: pts,
            color: Color {
                r: color.r,
                g: color.g,
                b: color.b,
                a: alpha,
            },
            width_mm: s.width().max(0.2),
            kind: if alpha < 128 {
                PenKind::Highlighter
            } else {
                PenKind::Pen
            },
        });
    }
    for child in ink.child_groups() {
        collect_ink(child, offset, out);
    }
}

fn utc_from(t: time::UtcDateTime) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(
            t.year(),
            t.month() as u32,
            t.day() as u32,
            t.hour() as u32,
            t.minute() as u32,
            t.second() as u32,
        )
        .single()
        .unwrap_or_else(chrono::Utc::now)
}

/// Build a `TypedPath` for the parser from a host path.
fn typed_path_from(path: &Path) -> TypedPath<'_> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        TypedPath::new(path.as_os_str().as_bytes(), typed_path::PathType::Unix)
    }
    #[cfg(not(unix))]
    {
        TypedPath::derive(&path.to_string_lossy())
    }
}

/// Recursively scan a folder for OneNote artifacts (used by "Open backup…").
pub fn find_onenote_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if OneFileKind::detect(&p).is_some() {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Compute the bounding box of all canvas content of a page (for auto-fit).
pub fn page_bounds(page: &Page) -> Rect {
    let mut r = Rect::default();
    let mut first = true;
    for b in &page.blocks {
        let bb = match b {
            Block::Outline { pos, width_mm, .. } => Rect::new(pos.x, pos.y, *width_mm, 40.0),
            Block::Ink { strokes, .. } => crate::canvas::strokes_bounds(strokes.iter()),
            Block::Image { pos, image, .. } => Rect::new(pos.x, pos.y, image.width_mm, image.height_mm),
            Block::File { pos, .. } => Rect::new(pos.x, pos.y, 40.0, 12.0),
            Block::Table { pos, .. } => Rect::new(pos.x, pos.y, 120.0, 60.0),
        };
        r = if first {
            first = false;
            bb
        } else {
            r.union(bb)
        };
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kinds() {
        assert_eq!(
            OneFileKind::detect(Path::new("a/b.one")),
            Some(OneFileKind::Section)
        );
        assert_eq!(
            OneFileKind::detect(Path::new("x.onetoc2")),
            Some(OneFileKind::Toc)
        );
        assert_eq!(
            OneFileKind::detect(Path::new("x.onepkg")),
            Some(OneFileKind::Package)
        );
        assert_eq!(OneFileKind::detect(Path::new("x.txt")), None);
    }

    #[test]
    fn tag_mapping() {
        assert_eq!(map_tag("Important"), NoteTag::Important);
        assert_eq!(map_tag("do later"), NoteTag::ToDo);
    }

    #[test]
    fn unique_import_dir_avoids_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let a = unique_import_dir(dir.path(), "Book (imported)");
        std::fs::create_dir_all(&a).unwrap();
        let b = unique_import_dir(dir.path(), "Book (imported)");
        assert_ne!(a, b);
        assert!(b.file_name().unwrap().to_string_lossy().ends_with(" 2"));
    }

    #[test]
    fn import_rejects_non_onenote_file() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("notes.txt");
        std::fs::write(&bogus, b"hello").unwrap();
        let mut store = Store::new();
        let err = import_any(&mut store, dir.path(), &bogus).unwrap_err();
        assert!(err.to_string().contains("not a OneNote file"));
    }
}
