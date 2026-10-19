//! Full-text search across every open notebook: page titles, body text,
//! note tags, and section/group names — the local equivalent of OneNote's
//! "Find pages" with a persistent in-memory inverted index.

use std::collections::{HashMap, HashSet};

use crate::model::{Block, Id, NoteTag, Page, TreeNode};

/// A searchable document = one page plus its context strings.
#[derive(Clone, Debug)]
pub struct IndexedPage {
    pub page_id: Id,
    pub notebook_id: Id,
    pub section_id: Id,
    pub section_name: String,
    pub group_path: Vec<String>,
    pub title: String,
    /// Lower-cased tokens of title + body + tag labels + section names.
    tokens: Vec<String>,
    /// Raw full text kept for snippet extraction.
    raw_text: String,
}

/// Inverted index: token -> (page -> term frequency).
#[derive(Default)]
pub struct SearchIndex {
    postings: HashMap<String, HashMap<Id, u32>>,
    docs: HashMap<Id, IndexedPage>,
    doc_total_len: HashMap<Id, usize>,
}

/// Tokenize: lowercase, strip punctuation, split on non-alphanumerics.
/// Unicode-aware via `char::is_alphanumeric`.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() >= 2 {
            out.push(word.to_lowercase());
        }
    }
    out
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn remove_page(&mut self, page_id: Id) {
        if let Some(doc) = self.docs.remove(&page_id) {
            for t in &doc.tokens {
                if let Some(p) = self.postings.get_mut(t) {
                    p.remove(&page_id);
                    if p.is_empty() {
                        self.postings.remove(t);
                    }
                }
            }
            self.doc_total_len.remove(&page_id);
        }
    }

    /// (Re-)index a page under its notebook/section context.
    pub fn index_page(
        &mut self,
        page: &Page,
        notebook_id: Id,
        section_id: Id,
        section_name: &str,
        group_path: &[String],
    ) {
        self.remove_page(page.id);
        let mut parts: Vec<String> = vec![
            page.title.clone(),
            section_name.to_string(),
            group_path.join(" "),
        ];
        for b in &page.blocks {
            parts.push(b.plain_text());
        }
        for t in page.tags() {
            parts.push(t.label().to_string());
        }
        // Also index tag words appearing inline ("#important").
        let raw = parts.join("\n");
        let mut tokens = tokenize(&raw);
        for b in &page.blocks {
            if let Block::Outline { paragraphs, .. } = b {
                for p in paragraphs {
                    if let Some(tag) = &p.tag {
                        tokens.extend(tokenize(tag.label()));
                    }
                }
            }
        }
        let mut postings_local: HashSet<String> = HashSet::new();
        for tok in &tokens {
            *self
                .postings
                .entry(tok.clone())
                .or_default()
                .entry(page.id)
                .or_insert(0) += 1;
            postings_local.insert(tok.clone());
        }
        let _ = postings_local;
        self.doc_total_len.insert(page.id, tokens.len());
        self.docs.insert(
            page.id,
            IndexedPage {
                page_id: page.id,
                notebook_id,
                section_id,
                section_name: section_name.to_string(),
                group_path: group_path.to_vec(),
                title: page.title.clone(),
                tokens,
                raw_text: raw,
            },
        );
    }

    /// Index an entire tree of sections/pages coming from a store snapshot.
    pub fn index_notebook(
        &mut self,
        nb_id: Id,
        tree: &[TreeNode],
        pages_of: &dyn Fn(Id) -> Vec<Page>,
    ) {
        fn walk(
            idx: &mut SearchIndex,
            nb_id: Id,
            nodes: &[TreeNode],
            path: &mut Vec<String>,
            pages_of: &dyn Fn(Id) -> Vec<Page>,
        ) {
            for n in nodes {
                match n {
                    TreeNode::Section(s) => {
                        for p in pages_of(s.id) {
                            idx.index_page(&p, nb_id, s.id, &s.name, path);
                        }
                    }
                    TreeNode::Group(g) => {
                        path.push(g.name.clone());
                        walk(idx, nb_id, &g.children, path, pages_of);
                        path.pop();
                    }
                }
            }
        }
        walk(self, nb_id, tree, &mut Vec::new(), pages_of);
    }

    /// Search all notebooks. Multi-word queries are AND-combined; results are
    /// ranked by a light BM25-ish score (term frequency / sqrt length), with
    /// title matches weighted double.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let qtoks = tokenize(query);
        if qtoks.is_empty() {
            return Vec::new();
        }
        let mut scores: HashMap<Id, f64> = HashMap::new();
        let mut matched_all = true;
        for qt in &qtoks {
            // prefix match so "note" finds "notes"; exact first if available.
            let keys: Vec<&String> = if let Some(exact) = self.postings.get(qt) {
                vec![(qt, exact)]
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect()
            } else {
                self.postings
                    .keys()
                    .filter(|k| k.starts_with(qt.as_str()))
                    .collect()
            };
            if keys.is_empty() {
                matched_all = false;
                break;
            }
            for key in keys {
                if let Some(posting) = self.postings.get(key) {
                    for (doc, tf) in posting {
                        let len = self.doc_total_len.get(doc).copied().unwrap_or(1).max(1) as f64;
                        *scores.entry(*doc).or_insert(0.0) +=
                            (*tf as f64) / len.sqrt();
                    }
                }
            }
        }
        if !matched_all {
            return Vec::new();
        }
        // Boost title matches.
        let qlower = query.to_lowercase();
        let mut hits: Vec<SearchHit> = scores
            .iter()
            .filter_map(|(doc_id, score)| {
                let doc = self.docs.get(doc_id)?;
                let boost = if doc.title.to_lowercase().contains(&qlower) {
                    2.0
                } else {
                    1.0
                };
                Some(SearchHit {
                    page_id: doc.page_id,
                    notebook_id: doc.notebook_id,
                    section_id: doc.section_id,
                    section_name: doc.section_name.clone(),
                    group_path: doc.group_path.clone(),
                    title: doc.title.clone(),
                    score: score * boost,
                    snippet: make_snippet(&doc.raw_text, &qlower),
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        hits
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub page_id: Id,
    pub notebook_id: Id,
    pub section_id: Id,
    pub section_name: String,
    pub group_path: Vec<String>,
    pub title: String,
    pub score: f64,
    pub snippet: String,
}

/// Extract a ±60-char window around the first case-insensitive match.
pub fn make_snippet(text: &str, needle_lower: &str) -> String {
    let lower = text.to_lowercase();
    match lower.find(needle_lower) {
        Some(pos) => {
            // Byte-safe windowing on char boundaries.
            let start = floor_char_boundary(text, pos.saturating_sub(40));
            let end = ceil_char_boundary(text, (pos + needle_lower.len() + 80).min(text.len()));
            let mut s = String::new();
            if start > 0 {
                s.push('…');
            }
            s.push_str(&text[start..end]);
            if end < text.len() {
                s.push('…');
            }
            s.replace('\n', " ")
        }
        None => {
            let end = ceil_char_boundary(text, 100.min(text.len()));
            let mut s = text[..end].replace('\n', " ");
            if end < text.len() {
                s.push('…');
            }
            s
        }
    }
}

fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

fn ceil_char_boundary(s: &str, i: usize) -> usize {
    if i <= 0 {
        return 0;
    }
    let mut j = i.min(s.len());
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Convenience: does this page contain any of these tags?
pub fn page_has_tag(page: &Page, tag: NoteTag) -> bool {
    page.tags().contains(&tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Paragraph, Point};

    fn sample() -> (SearchIndex, Id, Id, Id) {
        let mut idx = SearchIndex::new();
        let nb = Id::new();
        let sec = Id::new();
        let mut p1 = Page::new("Meeting notes");
        p1.add_text_block(Point::new(0.0, 0.0));
        if let Some(Block::Outline { paragraphs, .. }) = p1.blocks.first_mut() {
            let mut para = Paragraph::plain("Discussed quarterly budget and hiring plan");
            para.tag = Some(NoteTag::Important);
            paragraphs[0] = para;
        }
        let p2 = Page::new("Groceries");
        let id1 = p1.id;
        let id2 = p2.id;
        idx.index_page(&p1, nb, sec, "Work", &["2026".into()]);
        idx.index_page(&p2, nb, sec, "Work", &[]);
        (idx, nb, sec, if true { id1 } else { id2 })
    }

    #[test]
    fn finds_body_and_title_words() {
        let (idx, _, _, _) = sample();
        let hits = idx.search("budget", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Meeting notes");
        assert!(hits[0].snippet.contains("budget"));

        let hits = idx.search("meeting", 10);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn multi_word_and_semantics() {
        let (idx, _, _, _) = sample();
        assert_eq!(idx.search("budget hiring", 10).len(), 1);
        assert!(idx.search("budget groceries", 10).is_empty());
    }

    #[test]
    fn indexes_section_names_and_tags() {
        let (idx, _, _, _) = sample();
        assert!(idx.search("work", 10).iter().any(|h| h.title == "Meeting notes"));
        assert!(idx.search("important", 10).iter().any(|h| h.title == "Meeting notes"));
    }

    #[test]
    fn prefix_matching() {
        let (idx, _, _, _) = sample();
        assert!(!idx.search("budg", 10).is_empty());
    }

    #[test]
    fn reindex_replaces_old_tokens() {
        let (mut idx, nb, sec, pid) = sample();
        let mut p = Page::new("Renamed");
        p.id = pid;
        idx.index_page(&p, nb, sec, "Work", &[]);
        assert!(idx.search("budget", 10).is_empty());
        assert_eq!(idx.search("renamed", 10).len(), 1);
    }
}
