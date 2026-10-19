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

    pub fn notebook_dir(&self, id: Id) -> Option<PathBuf> {
        self.notebooks.iter().find(|n| n.id == id).map(|n| n.path.clone())
    }

    // -- sections -----------------------------------------------------------

    /// Create a brand-new empty section inside `notebook` and persist it.
    pub fn create_section(&mut self, notebook: Id, name: &str) -> Result<Id> {
        let dir = self.notebook_dir(notebook).ok_or(CoreError::NotFound("notebook".into()))?;
        let mut section = Section::new(name);
        section.origin = Some("leafnote".into());
        let sec_id = section.id;
        write_json(&dir.join("sections").join(format!("{sec_id}.json")), &section)?;
        self.sections.entry(dir.clone()).or_default().insert(sec_id, section);

        if let Some(nb) = self.notebook_mut(notebook) {
            nb.root.push(TreeNode::Section(crate::model::SectionRef {
                id: sec_id,
                name: name.into(),
                color: None,
                read_only: false,
            }));
            save_tree(nb)?;
        }
        Ok(sec_id)
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
struct NotebookMeta {
    version: u32,
    notebook: Notebook,
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
    let meta = NotebookMeta {
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
pub fn sanitize_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
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

/// Lightweight SHA-256 (no external deps) used for content-addressed media.
pub fn sha256_hex(data: &[u8]) -> String {
    use Sha256;
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Minimal, dependency-free SHA-256 implementation (FIPS 180-4).
struct Sha256;

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c257, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    fn digest(msg: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        // Padding.
        let bitlen = (msg.len() as u64) * 8;
        let mut data = msg.to_vec();
        data.push(0x80);
        while data.len() % 64 != 56 {
            data.push(0);
        }
        data.extend_from_slice(&bitlen.to_be_bytes());

        for block in data.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(Self::K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *x = x.wrapping_add(v);
            }
        }
        let mut out = [0u8; 32];
        for (i, w) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }
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
        assert_eq!(sanitize_folder_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_folder_name("   "), "untitled");
        assert_eq!(sanitize_folder_name("Work 📧"), "Work 📧");
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
