//! Local-first persistence.
//!
//! A leafNote **notebook is a plain folder** on your disk — no database, no
//! cloud, no lock-in. Layout (mirroring OneNote's own folder conventions):
//!
//! ```text
//! My Notebook/                 <- notebook folder
//! ├─ notebook.toml             <- name, id, tree.json inline? -> see below
//! ├─ tree.json                 <- Notebook metadata + section/group hierarchy
//! ├─ sections/
//! │   └─ <section-id>.json     <- Section index (page order, read-only flag)
//! ├─ pages/
//! │   └─ <page-id>.json        <- one file per page (the canvas document)
//! └─ media/
//!     └─ <sha256>.bin          <- content-addressed images & attachments
//! ```
//!
//! Writes are atomic (write to `*.tmp`, then rename) so a crash can never
//! leave a half-written page behind.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::model::{Id, Notebook, Page, Section, TreeNode};

/// Open collection of notebooks backed by the file system.
#[derive(Default)]
pub struct Store {
    notebooks: Vec<Notebook>,
    /// notebook dir -> (section id -> section index)
    sections: HashMap<PathBuf, HashMap<Id, Section>>,
    /// page id -> page (loaded lazily, cached here)
    pages: HashMap<Id, Page>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    // -- notebook discovery / opening --------------------------------------

    /// Scan `root` for immediate sub-folders that look like notebooks and
    /// open them all. Returns how many were opened.
    pub fn open_folder(&mut self, root: &Path) -> Result<usize> {
        let mut n = 0;
        if root.is_dir() {
            for entry in std::fs::read_dir(root)? {
                let path = entry?.path();
                if path.is_dir() && NotebookMeta::exists_at(&path) {
                    self.open_notebook(&path)?;
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    /// Open (or re-open) a single notebook directory.
    pub fn open_notebook(&mut self, dir: &Path) -> Result<Id> {
        let meta = NotebookMeta::load(dir)?;
        let id = meta.notebook.id;
        if !self.notebooks.iter().any(|nb| nb.id == id) {
            self.notebooks.push(meta.notebook);
        } else {
            return Ok(id);
        }
        let nb_dir = dir.to_path_buf();
        let mut map = HashMap::new();
        let sec_dir = nb_dir.join("sections");
        if sec_dir.is_dir() {
            for entry in std::fs::read_dir(&sec_dir)? {
                let p = entry?.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    let sec: Section = read_json(&p)?;
                    map.insert(sec.id, sec);
                }
            }
        }
        self.sections.insert(nb_dir, map);
        Ok(id)
    }

    pub fn notebooks(&self) -> &[Notebook] {
        &self.notebooks
    }

    pub fn notebook_mut(&mut self, id: Id) -> Option<&mut Notebook> {
        self.notebooks.iter_mut().find(|n| n.id == id)
    }

    /// Detach a notebook from the in-memory registry (the folder on disk is
    /// left untouched — closing never deletes data).
    pub fn close_notebook(&mut self, id: Id) {
        self.notebooks.retain(|n| n.id != id);
        self.pages.remove(&id);
    }

    pub fn notebook_dir(&self, id: Id) -> Option<PathBuf> {
        self.notebooks.iter().find(|n| n.id == id).map(|n| n.path.clone())
    }

    // -- sections -----------------------------------------------------------

    /// Create a brand-new empty section inside `notebook` and persist it.
    pub fn create_section(&mut self, notebook: Id, name: &str) -> Result<Id> {
        let sec = self.register_section(notebook, Section::new(name))?;
        Ok(sec.id)
    }

    /// Insert an already-built [`Section`] (created or imported) into the
    /// notebook's index files and tree, persisting both.
    pub fn register_section(&mut self, notebook: Id, section: Section) -> Result<Section> {
        let dir = self
            .notebook_dir(notebook)
            .ok_or_else(|| CoreError::NotFound(format!("notebook {notebook}")))?;
        let sec_id = section.id;
        std::fs::create_dir_all(dir.join("sections"))?;
        write_json(&dir.join("sections").join(format!("{sec_id}.json")), &section)?;
        self.sections
            .entry(dir)
            .or_default()
            .insert(sec_id, section.clone());

        let read_only = section.read_only;
        if let Some(nb) = self.notebook_mut(notebook) {
            nb.root.push(TreeNode::Section(crate::model::SectionRef {
                id: sec_id,
                name: section.name.clone(),
                color: section.color,
                read_only,
            }));
            save_tree(nb)?;
        }
        Ok(section)
    }

    pub fn section(&self, id: Id) -> Option<&Section> {
        for map in self.sections.values() {
            if let Some(s) = map.get(&id) {
                return Some(s);
            }
        }
        None
    }

    pub fn section_pages(&mut self, section_id: Id) -> Result<Vec<Page>> {
        let mut out = Vec::new();
        let dir = self.find_section_dir(section_id)?;
        let ids: Vec<Id> = self
            .sections
            .get(&dir)
            .and_then(|m| m.get(&section_id))
            .map(|s| s.page_ids.clone())
            .unwrap_or_default();
        for pid in ids {
            out.push(self.load_page(&dir, pid)?);
        }
        Ok(out)
    }

    fn find_section_dir(&self, section_id: Id) -> Result<PathBuf> {
        for (dir, map) in &self.sections {
            if map.contains_key(&section_id) {
                return Ok(dir.clone());
            }
        }
        Err(CoreError::NotFound(format!("section {section_id}")))
    }

    // -- pages ---------------------------------------------------------------

    /// Persist a page (atomic) and ensure it is registered in its section index.
    pub fn save_page(&mut self, section_id: Id, page: &Page) -> Result<()> {
        let dir = self.find_section_dir(section_id)?;
        write_json(
            &dir.join("pages").join(format!("{}.json", page.id)),
            page,
        )?;
        self.pages.insert(page.id, page.clone());
        let reg = self
            .sections
            .get_mut(&dir)
            .and_then(|m| m.get_mut(&section_id))
            .ok_or_else(|| CoreError::NotFound("section".into()))?;
        if !reg.page_ids.contains(&page.id) {
            reg.page_ids.push(page.id);
            write_json(&dir.join("sections").join(format!("{section_id}.json")), reg)?;
        }
        Ok(())
    }

    /// Create a fresh page inside a section.
    pub fn create_page(&mut self, section_id: Id, title: &str) -> Result<Id> {
        let page = Page::new(title);
        let id = page.id;
        self.save_page(section_id, &page)?;
        Ok(id)
    }

    pub fn page(&self, id: Id) -> Option<&Page> {
        self.pages.get(&id)
    }

    pub fn load_page(&mut self, notebook_dir: &Path, page_id: Id) -> Result<Page> {
        if let Some(p) = self.pages.get(&page_id) {
            return Ok(p.clone());
        }
        let p: Page = read_json(&notebook_dir.join("pages").join(format!("{page_id}.json")))?;
        self.pages.insert(p.id, p.clone());
        Ok(p)
    }

    /// Delete a page file and de-register it.
    pub fn delete_page(&mut self, section_id: Id, page_id: Id) -> Result<()> {
        let dir = self.find_section_dir(section_id)?;
        let path = dir.join("pages").join(format!("{page_id}.json"));
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.pages.remove(&page_id);
        if let Some(sec) = self
            .sections
            .get_mut(&dir)
            .and_then(|m| m.get_mut(&section_id))
        {
            sec.page_ids.retain(|p| *p != page_id);
            write_json(&dir.join("sections").join(format!("{section_id}.json")), sec)?;
        }
        Ok(())
    }

    // -- media ----------------------------------------------------------------

    /// Store a media blob content-addressed under `media/`; returns the hash.
    /// Identical bytes are stored once (dedup across imports).
    pub fn put_media(notebook_dir: &Path, bytes: &[u8]) -> Result<String> {
        let hash = sha256_hex(bytes);
        let dir = notebook_dir.join("media");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{hash}.bin"));
        if !path.exists() {
            write_atomic(&path, bytes)?;
        }
        Ok(hash)
    }

    pub fn get_media(notebook_dir: &Path, hash: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(notebook_dir.join("media").join(format!("{hash}.bin")))?)
    }

    /// Rename/recolour etc. — persist the notebook tree index.
    pub fn save_notebook_tree(&self, nb: &Notebook) -> Result<()> {
        save_tree(nb)
    }
}

// ---------------------------------------------------------------------------
// Notebook metadata (`tree.json`)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct NotebookMeta {
    version: u32,
    notebook: Notebook,
}

/// Public mirror of the `tree.json` envelope so the import engine can write
/// notebooks without duplicating the format definition.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NotebookMetaFile {
    pub version: u32,
    pub notebook: Notebook,
}

impl From<&NotebookMetaFile> for NotebookMeta {
    fn from(m: &NotebookMetaFile) -> Self {
        NotebookMeta {
            version: m.version,
            notebook: m.notebook.clone(),
        }
    }
}

impl From<&NotebookMeta> for NotebookMetaFile {
    fn from(m: &NotebookMeta) -> Self {
        NotebookMetaFile {
            version: m.version,
            notebook: m.notebook.clone(),
        }
    }
}

impl NotebookMeta {
    fn exists_at(dir: &Path) -> bool {
        dir.join("tree.json").is_file()
    }
    fn load(dir: &Path) -> Result<NotebookMeta> {
        let mut meta: NotebookMeta = read_json(&dir.join("tree.json"))?;
        meta.notebook.path = dir.to_path_buf();
        if meta.version > crate::FORMAT_VERSION {
            return Err(CoreError::Other(format!(
                "notebook was written by a newer leafNote (format {} > {})",
                meta.version,
                crate::FORMAT_VERSION
            )));
        }
        Ok(meta)
    }
}

fn save_tree(nb: &Notebook) -> Result<()> {
    let meta = NotebookMetaFile {
        version: crate::FORMAT_VERSION,
        notebook: nb.clone(),
    };
    write_json(&nb.path.join("tree.json"), &meta)
}

/// Create the on-disk skeleton for a brand-new notebook folder.
pub fn create_notebook(root: &Path, name: &str) -> Result<Notebook> {
    let dir = root.join(sanitize_folder_name(name));
    std::fs::create_dir_all(dir.join("sections"))?;
    std::fs::create_dir_all(dir.join("pages"))?;
    std::fs::create_dir_all(dir.join("media"))?;
    let mut nb = Notebook::new(name);
    nb.path = dir.clone();
    save_tree(&nb)?;
    Ok(nb)
}

/// Never let a notebook/section name escape its folder (path-traversal guard).
///
/// Order matters: replace path separators *first*, then trim surrounding
/// whitespace/dots — otherwise `"../../etc/passwd"` would survive trimming as
/// `.._.._etc_passwd` and could still climb directories.
pub fn sanitize_folder_name(name: &str) -> String {
    // Step 1: map every character that is unsafe in a file name to '_'.
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Step 2: trim whitespace and leading/trailing dots (kills "..", ".", " . ").
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "untitled".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

// ---------------------------------------------------------------------------
// Atomic IO helpers
// ---------------------------------------------------------------------------

pub(crate) fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let raw = serde_json::to_vec_pretty(value)?;
    write_atomic(path, &raw)
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read(path)?;
    Ok(serde_json::from_slice(&raw)?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Content-addressed media hashing (SHA-256, hex digest).
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Point;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sanitize_blocks_traversal() {
        // Separators are replaced first, then leading dots are trimmed away,
        // so the result can never contain a path component that climbs up.
        assert_eq!(sanitize_folder_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_folder_name(".."), "untitled");
        assert_eq!(sanitize_folder_name("."), "untitled");
        assert_eq!(sanitize_folder_name("   "), "untitled");
        assert_eq!(sanitize_folder_name(""), "untitled");
        assert_eq!(sanitize_folder_name("Work 📧"), "Work 📧");
        // Safety properties: no separators survive, and no path component of
        // the result is ever ".." or "." (so it cannot escape its parent dir).
        for name in [
            "../../etc/passwd",
            "..",
            ".",
            "...",
            "a/../../b",
            "C:\\Windows\\x",
            "foo/./bar",
        ] {
            let s = sanitize_folder_name(name);
            assert!(!s.contains('/') && !s.contains('\\'), "{name} -> {s}");
            assert!(!s.split(['/', '\\']).any(|c| c == ".." || c == "."), "{name} -> {s}");
        }
    }

    #[test]
    fn sanitize_collapses_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let nb = create_notebook(dir.path(), "Book").unwrap();
        // Two sections whose display names differ only by unsafe chars map to
        // the same folder slug — ids must stay distinct and slugs safe.
        let a = sanitize_folder_name("My:Notes");
        let b = sanitize_folder_name("My/Notes");
        assert_eq!(a, b); // same slug…
        assert!(!a.contains(':') && !a.contains('/')); // …but never unsafe
        let mut store = Store::new();
        let id = store.open_notebook(&nb.path).unwrap();
        let sa = store.create_section(id, "My:Notes").unwrap();
        let sb = store.create_section(id, "My/Notes").unwrap();
        assert_ne!(sa, sb);
        assert!(store.section(sa).is_some());
        assert!(store.section(sb).is_some());
    }

    #[test]
    fn full_crud_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = create_notebook(dir.path(), "Test Book").unwrap();
        let mut store = Store::new();
        let id = store.open_notebook(&nb.path).unwrap();
        nb.id = nb.id; // keep clippy quiet about unused mut semantics
        let _ = nb;

        let sec = store.create_section(id, "Quick Notes").unwrap();
        let mut page = Page::new("First page");
        page.add_text_block(Point::new(10.0, 10.0));
        let pid = page.id;
        store.save_page(sec, &page).unwrap();

        let media = Store::put_media(&dir.path().join("Test Book"), b"hello").unwrap();
        assert_eq!(Store::get_media(&dir.path().join("Test Book"), &media).unwrap(), b"hello");

        // Reopen from scratch.
        let mut store2 = Store::new();
        store2.open_folder(dir.path()).unwrap();
        let pages = store2.section_pages(sec).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, pid);
        assert_eq!(pages[0].title, "First page");
    }
}
