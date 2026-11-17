//! Pure text decoding, alignment, and staged edits. Nothing in this module writes files.
use anyhow::{Result, bail};
use similar::{Algorithm, DiffTag, capture_diff_slices_deadline};
use std::{
    ops::Range,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Same,
    Minor,
    LeftOnly,
    RightOnly,
    Error,
    Different,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Same => "identical",
            Self::Minor => "minor",
            Self::Different => "different",
            Self::LeftOnly => "only left",
            Self::RightOnly => "only right",
            Self::Error => "error",
        }
    }

    pub fn mark(self) -> &'static str {
        match self {
            Self::Same => "=",
            Self::Minor => "≈",
            Self::Different => "≠",
            Self::LeftOnly => "←",
            Self::RightOnly => "→",
            Self::Error => "!",
        }
    }

    pub fn is_difference(self) -> bool {
        !matches!(self, Self::Same | Self::Minor)
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    pub lines: Vec<String>,
    pub final_newline: bool,
    pub bom: bool,
    pub eol: String,
    pub encoding: String,
    pub readonly: bool,
    pub binary: bool,
    pub exists: bool,
    /// Bytes observed when opening or last saving this side; also used for conflict checks.
    pub baseline: Option<Vec<u8>>,
    pending_create: bool,
}

impl Document {
    pub fn from_bytes(bytes: Option<Vec<u8>>) -> Self {
        let mut document = Self {
            lines: Vec::new(),
            final_newline: true,
            bom: false,
            eol: "\n".into(),
            encoding: "UTF-8".into(),
            readonly: false,
            binary: false,
            exists: bytes.is_some(),
            baseline: bytes,
            pending_create: false,
        };
        let Some(raw) = document.baseline.as_deref() else {
            return document;
        };
        let text = if raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(&[0xfe, 0xff]) {
            let encoding = if raw[0] == 0xff {
                encoding_rs::UTF_16LE
            } else {
                encoding_rs::UTF_16BE
            };
            document.encoding = if raw[0] == 0xff {
                "UTF-16 LE"
            } else {
                "UTF-16 BE"
            }
            .into();
            document.readonly = true;
            encoding
                .decode_without_bom_handling(&raw[2..])
                .0
                .into_owned()
        } else {
            if raw.iter().take(8000).any(|&byte| byte == 0) {
                document.binary = true;
                document.readonly = true;
                document.encoding = "binary".into();
                return document;
            }
            document.bom = raw.starts_with(&[0xef, 0xbb, 0xbf]);
            let body = if document.bom { &raw[3..] } else { raw };
            match std::str::from_utf8(body) {
                Ok(text) => text.to_owned(),
                Err(_) => {
                    document.encoding = "Windows-1252".into();
                    document.readonly = true;
                    encoding_rs::WINDOWS_1252
                        .decode_without_bom_handling(body)
                        .0
                        .into_owned()
                }
            }
        };
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        let cr = text.matches('\r').count() - crlf;
        document.eol = if cr > crlf && cr > lf {
            "\r"
        } else if crlf > lf {
            "\r\n"
        } else {
            "\n"
        }
        .into();
        (document.lines, document.final_newline) = split_text(&text);
        // Empty files have no observable final-newline state.
        if text.is_empty() {
            document.final_newline = true;
        }
        document
    }

    pub fn encode(&self) -> Vec<u8> {
        // Non-UTF-8 and binary files are available for comparison and whole-file copying only.
        if self.readonly {
            return self.baseline.clone().unwrap_or_default();
        }
        let mut text = self.lines.join(&self.eol);
        if !self.lines.is_empty() && self.final_newline {
            text.push_str(&self.eol);
        }
        let mut out = Vec::with_capacity(text.len() + 3);
        if self.bom {
            out.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        out.extend_from_slice(text.as_bytes());
        out
    }

    pub fn dirty(&self) -> bool {
        if self.readonly {
            return false;
        }
        let Some(bytes) = &self.baseline else {
            return self.pending_create || !self.lines.is_empty();
        };
        // Compare logical content so opening a file with mixed line endings never dirties it.
        let original = Self::from_bytes(Some(bytes.clone()));
        self.lines != original.lines
            || self.final_newline != original.final_newline
            || self.bom != original.bom
            || self.eol != original.eol
    }

    pub fn adopt_format(&mut self, other: &Self) {
        if !self.exists && other.exists {
            self.eol.clone_from(&other.eol);
            self.bom = other.bom;
        }
    }
}

fn split_text(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let final_newline = normalized.ends_with('\n');
    let mut lines: Vec<_> = normalized.split('\n').map(str::to_owned).collect();
    if final_newline {
        lines.pop();
    }
    (lines, final_newline)
}

fn normalize_whitespace(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn compare_bytes(left: &[u8], right: &[u8], ignore_whitespace: bool) -> Status {
    if left == right {
        return Status::Same;
    }
    let left = Document::from_bytes(Some(left.to_vec()));
    let right = Document::from_bytes(Some(right.to_vec()));
    if left.binary || right.binary || left.lines.len() != right.lines.len() {
        return Status::Different;
    }
    if left.lines.iter().zip(&right.lines).all(|(a, b)| {
        a == b || (ignore_whitespace && normalize_whitespace(a) == normalize_whitespace(b))
    }) {
        Status::Minor
    } else {
        Status::Different
    }
}

#[derive(Clone, Debug)]
pub struct DiffRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub hunk: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub rows: Range<usize>,
    pub left: Range<usize>,
    pub right: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<Hunk>,
}

impl Diff {
    pub fn new(left: &[String], right: &[String], ignore_whitespace: bool) -> Self {
        let (left_keys, right_keys);
        let (a, b) = if ignore_whitespace {
            left_keys = left
                .iter()
                .map(|s| normalize_whitespace(s))
                .collect::<Vec<_>>();
            right_keys = right
                .iter()
                .map(|s| normalize_whitespace(s))
                .collect::<Vec<_>>();
            (left_keys.as_slice(), right_keys.as_slice())
        } else {
            (left, right)
        };
        // A bounded patience diff keeps the UI usable even for unrelated generated files.
        let ops = capture_diff_slices_deadline(
            Algorithm::Patience,
            a,
            b,
            Some(Instant::now() + Duration::from_secs(1)),
        );
        let mut diff = Self::default();
        for op in ops {
            let (tag, a, b) = op.as_tag_tuple();
            if tag == DiffTag::Equal {
                for offset in 0..a.len() {
                    diff.rows.push(DiffRow {
                        left: Some(a.start + offset),
                        right: Some(b.start + offset),
                        hunk: None,
                    });
                }
            } else {
                let hunk = diff.hunks.len();
                let first = diff.rows.len();
                for offset in 0..a.len().max(b.len()) {
                    diff.rows.push(DiffRow {
                        left: (offset < a.len()).then_some(a.start + offset),
                        right: (offset < b.len()).then_some(b.start + offset),
                        hunk: Some(hunk),
                    });
                }
                diff.hunks.push(Hunk {
                    rows: first..diff.rows.len(),
                    left: a,
                    right: b,
                });
            }
        }
        diff
    }

    pub fn documents(left: &Document, right: &Document, ignore_whitespace: bool) -> Self {
        let mut diff = Self::new(&left.lines, &right.lines, ignore_whitespace);
        if left.final_newline != right.final_newline
            && !diff
                .hunks
                .last()
                .is_some_and(|h| h.left.end == left.lines.len() && h.right.end == right.lines.len())
        {
            let row = diff.rows.len();
            diff.rows.push(DiffRow {
                left: None,
                right: None,
                hunk: Some(diff.hunks.len()),
            });
            diff.hunks.push(Hunk {
                rows: row..row + 1,
                left: left.lines.len()..left.lines.len(),
                right: right.lines.len()..right.lines.len(),
            });
        }
        diff
    }
}

#[derive(Debug)]
struct Change {
    side: usize,
    lines: Vec<String>,
    final_newline: bool,
    pending_create: bool,
    cursor: usize,
}

#[derive(Debug)]
pub struct Session {
    /// Invalidates derived syntax state whenever a file is opened or changed.
    pub revision: u64,
    pub docs: [Document; 2],
    pub diff: Diff,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub horizontal: usize,
    pub path: PathBuf,
    pub stale: [bool; 2],
    history: Vec<Change>,
    ignore_whitespace: bool,
}

impl Session {
    pub fn new(path: PathBuf, mut docs: [Document; 2], ignore_whitespace: bool) -> Self {
        if !docs[0].exists {
            let (left, right) = docs.split_at_mut(1);
            left[0].adopt_format(&right[0]);
        } else if !docs[1].exists {
            let (left, right) = docs.split_at_mut(1);
            right[0].adopt_format(&left[0]);
        }
        let diff = Diff::documents(&docs[0], &docs[1], ignore_whitespace);
        let cursor = diff.hunks.first().map_or(0, |h| h.rows.start);
        Self {
            revision: next_revision(),
            docs,
            diff,
            cursor,
            anchor: None,
            horizontal: 0,
            path,
            stale: [false; 2],
            history: Vec::new(),
            ignore_whitespace,
        }
    }

    pub fn refresh_diff(&mut self, ignore_whitespace: bool) {
        self.revision = next_revision();
        self.ignore_whitespace = ignore_whitespace;
        self.diff = Diff::documents(&self.docs[0], &self.docs[1], ignore_whitespace);
        self.cursor = self.cursor.min(self.diff.rows.len().saturating_sub(1));
        self.anchor = None;
    }

    /// Map a half-open range of aligned rows to lines, including insert positions on an empty side.
    pub fn range(&self, side: usize, rows: Range<usize>) -> Range<usize> {
        let index = |row: &DiffRow| if side == 0 { row.left } else { row.right };
        let start = rows.start.min(self.diff.rows.len());
        let end = rows.end.min(self.diff.rows.len()).max(start);
        let mut first = None;
        let mut last = 0;
        for row in &self.diff.rows[start..end] {
            if let Some(line) = index(row) {
                first.get_or_insert(line);
                last = line + 1;
            }
        }
        if let Some(first) = first {
            return first..last;
        }
        let insertion = self.diff.rows[..start]
            .iter()
            .rev()
            .find_map(index)
            .map_or(0, |line| line + 1);
        insertion..insertion
    }

    fn remember(&mut self, side: usize) {
        self.history.push(Change {
            side,
            lines: self.docs[side].lines.clone(),
            final_newline: self.docs[side].final_newline,
            pending_create: self.docs[side].pending_create,
            cursor: self.cursor,
        });
        if self.history.len() > 200 {
            self.history.remove(0);
        }
    }

    pub fn merge(
        &mut self,
        to: usize,
        all: bool,
        ignore_whitespace: bool,
        jump_after_merge: bool,
    ) -> Result<()> {
        if to > 1 {
            bail!("Invalid side");
        }
        if self.docs[to].readonly {
            bail!("This side is read-only; use whole-file copying from the tree");
        }
        let from = 1 - to;
        if self.docs[from].binary {
            bail!("Binary files can only be copied as whole files from the tree");
        }
        let mut next_hunk = None;
        let (source, target) = if all {
            (0..self.docs[from].lines.len(), 0..self.docs[to].lines.len())
        } else if let Some(anchor) = self.anchor {
            let rows = anchor.min(self.cursor)..anchor.max(self.cursor) + 1;
            (self.range(from, rows.clone()), self.range(to, rows))
        } else {
            let Some(hunk_index) = self.diff.rows.get(self.cursor).and_then(|row| row.hunk) else {
                bail!("No difference selected; use n or N to jump to one");
            };
            next_hunk = Some(hunk_index);
            let h = &self.diff.hunks[hunk_index];
            if to == 1 {
                (h.left.clone(), h.right.clone())
            } else {
                (h.right.clone(), h.left.clone())
            }
        };
        let at_end =
            source.end == self.docs[from].lines.len() && target.end == self.docs[to].lines.len();
        let final_newline = if at_end {
            self.docs[from].final_newline
        } else {
            self.docs[to].final_newline
        };
        let replacement = self.docs[from].lines[source].to_vec();
        let create = !self.docs[to].exists && (self.docs[from].exists || !replacement.is_empty());
        if self.docs[to].lines[target.clone()] == replacement
            && self.docs[to].final_newline == final_newline
            && !create
        {
            return Ok(());
        }
        self.remember(to);
        self.docs[to].lines.splice(target, replacement);
        self.docs[to].final_newline = final_newline;
        self.docs[to].pending_create |= create;
        self.refresh_diff(ignore_whitespace);
        if jump_after_merge
            && let Some(index) = next_hunk
            && let Some(next) = self
                .diff
                .hunks
                .get(index)
                .or_else(|| self.diff.hunks.last())
        {
            self.cursor = next.rows.start;
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(change) = self.history.pop() else {
            return false;
        };
        self.docs[change.side].lines = change.lines;
        self.docs[change.side].final_newline = change.final_newline;
        self.docs[change.side].pending_create = change.pending_create;
        self.cursor = change.cursor;
        self.refresh_diff(self.ignore_whitespace);
        true
    }

    pub fn replace(
        &mut self,
        side: usize,
        range: Range<usize>,
        text: &str,
        ignore_whitespace: bool,
    ) -> Result<()> {
        if side > 1 {
            bail!("Invalid side");
        }
        if self.docs[side].readonly {
            bail!("This side is read-only");
        }
        if range.start > range.end || range.end > self.docs[side].lines.len() {
            bail!("Invalid edit range");
        }
        let (replacement, newline) = split_text(text);
        let at_end = range.end == self.docs[side].lines.len();
        let final_newline = if at_end {
            newline || (range.start == 0 && replacement.is_empty())
        } else {
            self.docs[side].final_newline
        };
        if self.docs[side].lines[range.clone()] == replacement
            && self.docs[side].final_newline == final_newline
        {
            return Ok(());
        }
        self.remember(side);
        self.docs[side].lines.splice(range, replacement);
        self.docs[side].final_newline = final_newline;
        self.docs[side].pending_create = !self.docs[side].exists;
        self.refresh_diff(ignore_whitespace);
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.docs.iter().any(Document::dirty)
    }
}

fn next_revision() -> u64 {
    static REVISION: AtomicU64 = AtomicU64::new(1);
    REVISION.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &[u8]) -> Document {
        Document::from_bytes(Some(text.to_vec()))
    }
    fn session(left: &[u8], right: &[u8]) -> Session {
        Session::new("file.txt".into(), [doc(left), doc(right)], false)
    }

    #[test]
    fn round_trip_utf8_bom_and_line_endings() {
        for input in [
            b"\xef\xbb\xbfhello\r\nworld\r\n".as_slice(),
            b"hello\nworld",
            b"",
            b"\n",
            b"a\rb\r",
        ] {
            let document = doc(input);
            assert_eq!(document.encode(), input);
            assert!(!document.dirty());
        }
        assert!(!doc(b"one\r\ntwo\nthree\r\n").dirty());
    }

    #[test]
    fn legacy_encodings_and_binary_are_read_only() {
        let utf16 = doc(&[0xff, 0xfe, b'a', 0, b'\n', 0]);
        assert_eq!(utf16.lines, ["a"]);
        assert!(utf16.readonly);
        let cp1252 = doc(b"caf\xe9\n");
        assert_eq!(cp1252.lines, ["café"]);
        assert!(cp1252.readonly);
        let binary = doc(b"data\0tail");
        assert!(binary.binary && binary.readonly);
        assert_eq!(binary.encode(), b"data\0tail");
    }

    #[test]
    fn classifications_match_html_whitespace_and_encoding_rules() {
        assert_eq!(compare_bytes(b"x\n", b"x\n", false), Status::Same);
        assert_eq!(
            compare_bytes(b"x\r\n", b"\xef\xbb\xbfx\n", false),
            Status::Minor
        );
        assert_eq!(compare_bytes(b"x", b"x\n", false), Status::Minor);
        assert_eq!(
            compare_bytes(b" a\t b\n", b"a b\n", false),
            Status::Different
        );
        assert_eq!(compare_bytes(b" a\t b\n", b"a b\n", true), Status::Minor);
        assert_eq!(compare_bytes(b"a\0", b"b\0", true), Status::Different);
    }

    #[test]
    fn aligns_insertions_deletions_and_replacements() {
        let s = session(b"start\nleft\nextra\nend\n", b"start\nright\nend\n");
        assert_eq!(s.diff.hunks.len(), 1);
        let h = &s.diff.hunks[0];
        assert_eq!(h.left, 1..3);
        assert_eq!(h.right, 1..2);
        assert_eq!(s.diff.rows[h.rows.end - 1].right, None);
        assert_eq!(s.range(1, h.rows.end - 1..h.rows.end), 2..2);
    }

    #[test]
    fn section_merge_is_staged_preserves_target_format_and_undoes() {
        let mut s = session(b"start\nnew\nend\n", b"\xef\xbb\xbfstart\r\nold\r\nend\r\n");
        s.merge(1, false, false, true).unwrap();
        assert!(s.docs[1].dirty());
        assert_eq!(s.docs[1].encode(), b"\xef\xbb\xbfstart\r\nnew\r\nend\r\n");
        assert_eq!(
            s.docs[1].baseline.as_deref(),
            Some(b"\xef\xbb\xbfstart\r\nold\r\nend\r\n".as_slice())
        );
        assert!(s.undo());
        assert!(!s.dirty());
        assert!(!s.undo());
    }

    #[test]
    fn selected_rows_merge_only_the_selected_part_of_a_hunk() {
        let mut s = session(b"a\nb\nc\n", b"x\ny\nz\n");
        s.cursor = 1;
        s.anchor = Some(1);
        s.merge(1, false, false, true).unwrap();
        assert_eq!(s.docs[1].lines, ["x", "b", "z"]);
        assert!(s.anchor.is_none());
    }

    #[test]
    fn whole_file_and_eof_hunk_transfer_final_newline() {
        let mut s = session(b"same", b"same\n");
        assert_eq!(s.diff.hunks.len(), 1);
        assert!(s.diff.rows[s.cursor].left.is_none());
        s.merge(1, false, false, true).unwrap();
        assert_eq!(s.docs[1].encode(), b"same");
        assert!(s.diff.hunks.is_empty());
        s.undo();
        s.merge(0, true, false, true).unwrap();
        assert_eq!(s.docs[0].encode(), b"same\n");
    }

    #[test]
    fn missing_targets_adopt_format_and_empty_files_can_be_created() {
        let mut s = Session::new(
            "new.txt".into(),
            [doc(b"\xef\xbb\xbfnew\r\n"), Document::from_bytes(None)],
            false,
        );
        assert!(!s.docs[1].dirty());
        s.merge(1, true, false, true).unwrap();
        assert_eq!(s.docs[1].encode(), b"\xef\xbb\xbfnew\r\n");
        assert!(s.docs[1].dirty());
        s.undo();
        assert!(!s.docs[1].dirty());
        let mut empty = Session::new(
            "empty".into(),
            [doc(b""), Document::from_bytes(None)],
            false,
        );
        empty.merge(1, true, false, true).unwrap();
        assert!(empty.docs[1].dirty());
    }

    #[test]
    fn undo_after_save_keeps_latest_disk_baseline() {
        let mut s = session(b"new\n", b"old\n");
        s.merge(1, true, false, true).unwrap();
        s.docs[1].baseline = Some(s.docs[1].encode());
        assert!(!s.docs[1].dirty());
        s.undo();
        assert_eq!(s.docs[1].lines, ["old"]);
        assert!(s.docs[1].dirty());
        assert_eq!(s.docs[1].baseline.as_deref(), Some(b"new\n".as_slice()));
    }

    #[test]
    fn editing_removes_lines_and_protects_read_only_documents() {
        let mut s = session(b"first\nlast\n", b"abc\0");
        s.replace(0, 1..2, "", false).unwrap();
        assert_eq!(s.docs[0].encode(), b"first");
        s.undo();
        s.replace(0, 0..1, "replacement\n", false).unwrap();
        assert_eq!(s.docs[0].encode(), b"replacement\nlast\n");
        assert!(s.replace(1, 0..0, "text", false).is_err());
        assert!(s.merge(1, true, false, true).is_err());
    }

    #[test]
    fn merging_each_hunk_reconstructs_source_for_repeated_and_empty_lines() {
        let samples: &[&[u8]] = &[
            b"",
            b"\n",
            b"a",
            b"a\n",
            b"a\na\n",
            b"a\nb\na\n",
            b"b\na\nb",
            b"\n\na\n\n",
            b"a\nnew\nb\nend\n",
        ];
        for &left in samples {
            for &right in samples {
                let mut s = session(left, right);
                let mut remaining = left.len() + right.len() + 2;
                while let Some(hunk) = s.diff.hunks.first() {
                    assert!(
                        remaining > 0,
                        "merging failed to converge: {left:?} / {right:?}"
                    );
                    remaining -= 1;
                    s.cursor = hunk.rows.start;
                    s.merge(1, false, false, true).unwrap();
                }
                assert_eq!(s.docs[0].lines, s.docs[1].lines);
                assert_eq!(s.docs[0].final_newline, s.docs[1].final_newline);
            }
        }
    }
}
