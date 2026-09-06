//! SQ5 — passage chunking for vector search.
//!
//! The indexer embeds each document's whole body as ONE vector, but the
//! bge models truncate at 512 tokens, so semantic search only "sees" the
//! first ~400 words of a long artifact (the BM25 arm indexes the full
//! body, which is why hybrid still scores well). This module splits the
//! body into overlapping word windows small enough that each embeds in
//! full; the indexer embeds each chunk into a sibling `artifact_chunks`
//! lance table, and search vector-queries the chunks then max-pools to a
//! per-document score.
//!
//! Pure + deterministic (a function of the parsed text only — no clock,
//! no map iteration order), so a re-index of an unchanged file produces
//! identical chunks.

/// Target words per body chunk. ~280 words ≈ ~365 BGE tokens (≈1.3
/// tokens/word), a safe margin under the 512-token cap including the
/// `[CLS]`/`[SEP]` overhead.
pub const DEFAULT_CHUNK_WORDS: usize = 280;

/// Words of overlap between adjacent body chunks, so a passage straddling
/// a window boundary still lands wholly inside at least one chunk.
pub const DEFAULT_OVERLAP_WORDS: usize = 60;

/// One passage to embed. `idx` is stable per document (0-based, in reading
/// order); `text` is what gets embedded (and later fed to the reranker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub idx: u32,
    pub text: String,
}

/// Split a document into embeddable chunks.
///
/// Chunk 0 is always `title` + `headings` (a short, high-signal passage
/// that lifts short queries and keeps body-less artifacts representable).
/// The body is then sliced into `chunk_words`-word windows that overlap by
/// `overlap_words`. Returns an empty vec only when title, headings, AND
/// body are all empty.
pub fn chunk_document(
    title: &str,
    headings: &str,
    body: &str,
    chunk_words: usize,
    overlap_words: usize,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut idx: u32 = 0;

    // Chunk 0 — title + headings.
    let head = format!("{}\n{}", title.trim(), headings.trim());
    let head = head.trim();
    if !head.is_empty() {
        chunks.push(Chunk {
            idx,
            text: head.to_string(),
        });
        idx += 1;
    }

    // Body windows.
    let words: Vec<&str> = body.split_whitespace().collect();
    if !words.is_empty() {
        // Guard against a degenerate step (overlap >= window) that would
        // never advance: always make progress.
        let window = chunk_words.max(1);
        let step = window.saturating_sub(overlap_words).max(1);
        let mut start = 0usize;
        loop {
            let end = (start + window).min(words.len());
            chunks.push(Chunk {
                idx,
                text: words[start..end].join(" "),
            });
            idx += 1;
            if end == words.len() {
                break;
            }
            start += step;
        }
    }

    chunks
}

/// Chunk with the module defaults.
pub fn chunk_document_default(title: &str, headings: &str, body: &str) -> Vec<Chunk> {
    chunk_document(
        title,
        headings,
        body,
        DEFAULT_CHUNK_WORDS,
        DEFAULT_OVERLAP_WORDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> String {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn chunk_zero_is_title_plus_headings() {
        let c = chunk_document_default("My Title", "Heading One Heading Two", "");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].idx, 0);
        assert!(c[0].text.contains("My Title"));
        assert!(c[0].text.contains("Heading One"));
    }

    #[test]
    fn short_body_is_a_single_window() {
        let c = chunk_document_default("T", "", &words(50));
        // chunk 0 (title) + one body window.
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].idx, 1);
        assert_eq!(c[1].text.split_whitespace().count(), 50);
    }

    #[test]
    fn long_body_splits_into_overlapping_windows() {
        // 600 words, window 280, overlap 60 → step 220. Windows:
        // [0,280), [220,500), [440,600) → 3 body chunks.
        let c = chunk_document("T", "", &words(600), 280, 60);
        let body_chunks = &c[1..]; // skip chunk 0 (title)
        assert_eq!(body_chunks.len(), 3, "expected 3 body windows: {}", c.len());
        // Adjacent windows overlap: last 60 words of chunk 1 == first 60 of chunk 2.
        let w1: Vec<&str> = body_chunks[0].text.split_whitespace().collect();
        let w2: Vec<&str> = body_chunks[1].text.split_whitespace().collect();
        assert_eq!(
            &w1[w1.len() - 60..],
            &w2[..60],
            "windows must overlap by 60"
        );
        // idx is contiguous from 0.
        for (i, ch) in c.iter().enumerate() {
            assert_eq!(ch.idx as usize, i);
        }
    }

    #[test]
    fn last_window_is_not_duplicated() {
        // 280 words exactly → one window, no trailing empty chunk.
        let c = chunk_document("T", "", &words(280), 280, 60);
        assert_eq!(c.len(), 2); // title + 1 body window
    }

    #[test]
    fn empty_document_yields_no_chunks() {
        assert!(chunk_document_default("", "", "").is_empty());
        assert!(chunk_document_default("   ", "  ", "  \n ").is_empty());
    }

    #[test]
    fn body_only_still_chunks() {
        let c = chunk_document_default("", "", &words(10));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].idx, 0);
    }

    #[test]
    fn is_deterministic() {
        let a = chunk_document_default("T", "H", &words(1000));
        let b = chunk_document_default("T", "H", &words(1000));
        assert_eq!(a, b);
    }

    #[test]
    fn degenerate_overlap_still_terminates() {
        // overlap >= window would make step 0; the guard forces progress.
        let c = chunk_document("", "", &words(100), 10, 50);
        assert!(!c.is_empty());
        // contiguous idx, terminates (no infinite loop = test returns).
        for (i, ch) in c.iter().enumerate() {
            assert_eq!(ch.idx as usize, i);
        }
    }
}
