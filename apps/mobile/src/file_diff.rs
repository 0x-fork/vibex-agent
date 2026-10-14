//! A file operation as something a reader can look at.
//!
//! The timeline already carries what the Agent did to a file, but a phone can
//! only show a change if it is turned into lines: the payload's two snapshots
//! are as large as the files they describe, and the desktop reads them the same
//! way — through the operation's own patch.
//!
//! `FileOperationPayload::canonical_patch` synthesizes a unified patch when the
//! adapter sent snapshots instead of one, so this module has a single input to
//! parse and never has to diff anything itself. That also keeps the phone and
//! the desktop agreeing on what a change is, since both read the same patch.
//!
//! The preview is bounded: an Agent that rewrites a lock file can produce a
//! hundred thousand lines, and a phone that lays all of them out stops
//! scrolling. The head and tail are kept and the middle is reported as omitted,
//! which is what the desktop's preview does too.

use vibex_core::FileOperationPayload;
use vibex_desktop_model::{UnifiedDiffLineKind, parse_unified_diff};

/// How many lines one preview keeps, before the middle is dropped.
pub const FILE_DIFF_PREVIEW_LINES: usize = 400;
/// The longest line a preview keeps, in bytes. A minified bundle is one line
/// and would otherwise be laid out whole on every frame.
const FILE_DIFF_MAX_LINE_BYTES: usize = 2_000;

/// Which side of the change a previewed line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDiffLineKind {
    Context,
    Added,
    Removed,
    /// A run of lines the preview did not keep. The count is `None` at a hunk
    /// boundary, where the patch itself is what left the lines out and their
    /// number is not known — the reader is told only that something is missing.
    Omitted(Option<usize>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffLine {
    pub kind: FileDiffLineKind,
    pub text: String,
}

/// One file operation's change, bounded and counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffPreview {
    pub lines: Vec<FileDiffLine>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub omitted_lines: usize,
}

impl FileDiffPreview {
    /// Whether there is anything to draw.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Projects one file operation into a bounded preview.
///
/// `None` means the operation carries no change this build can read: a read, or
/// a patch in a format that is not a unified diff.
pub fn project(operation: &FileOperationPayload) -> Option<FileDiffPreview> {
    let patch = operation.canonical_patch()?;
    let files = parse_unified_diff(&patch.text);
    if files.is_empty() {
        return None;
    }
    let mut builder = PreviewBuilder::new(FILE_DIFF_PREVIEW_LINES);
    let mut saw_hunk = false;
    for file in files {
        for line in file.lines {
            match line.kind {
                UnifiedDiffLineKind::Hunk => {
                    // One marker per hunk boundary, and none before the first:
                    // the header already says where the change starts.
                    if saw_hunk {
                        builder.push(FileDiffLineKind::Omitted(None), "");
                    }
                    saw_hunk = true;
                }
                UnifiedDiffLineKind::Add => builder.push(FileDiffLineKind::Added, &line.content),
                UnifiedDiffLineKind::Delete => {
                    builder.push(FileDiffLineKind::Removed, &line.content)
                }
                UnifiedDiffLineKind::Context => {
                    builder.push(FileDiffLineKind::Context, &line.content)
                }
                // The file headers and `diff --git` lines are already implied
                // by the row this preview was opened from.
                UnifiedDiffLineKind::Meta => {}
            }
        }
    }
    Some(builder.finish())
}

/// Counts one operation's change without keeping its lines.
///
/// The file-changes card wants `+N −M` for every row it lists, and a row that
/// is never opened should not hold a preview in memory to answer that.
pub fn line_counts(operation: &FileOperationPayload) -> Option<(usize, usize)> {
    let patch = operation.canonical_patch()?;
    let mut added = 0;
    let mut removed = 0;
    for file in parse_unified_diff(&patch.text) {
        for line in file.lines {
            match line.kind {
                UnifiedDiffLineKind::Add => added += 1,
                UnifiedDiffLineKind::Delete => removed += 1,
                _ => {}
            }
        }
    }
    (added > 0 || removed > 0).then_some((added, removed))
}

/// Keeps the first and last lines of a change and counts the rest.
struct PreviewBuilder {
    head: Vec<FileDiffLine>,
    tail: std::collections::VecDeque<FileDiffLine>,
    head_limit: usize,
    tail_limit: usize,
    total: usize,
    added_lines: usize,
    removed_lines: usize,
}

impl PreviewBuilder {
    fn new(limit: usize) -> Self {
        let head_limit = limit / 2;
        Self {
            head: Vec::with_capacity(head_limit),
            tail: std::collections::VecDeque::with_capacity(limit.saturating_sub(head_limit)),
            head_limit,
            tail_limit: limit.saturating_sub(head_limit),
            total: 0,
            added_lines: 0,
            removed_lines: 0,
        }
    }

    fn push(&mut self, kind: FileDiffLineKind, text: &str) {
        match kind {
            FileDiffLineKind::Added => self.added_lines += 1,
            FileDiffLineKind::Removed => self.removed_lines += 1,
            FileDiffLineKind::Context | FileDiffLineKind::Omitted(_) => {}
        }
        self.total += 1;
        let line = FileDiffLine {
            kind,
            text: bounded_line(text),
        };
        if self.head.len() < self.head_limit {
            self.head.push(line);
        } else if self.tail_limit > 0 {
            if self.tail.len() == self.tail_limit {
                self.tail.pop_front();
            }
            self.tail.push_back(line);
        }
    }

    fn finish(self) -> FileDiffPreview {
        let omitted_lines = self.total.saturating_sub(self.head.len() + self.tail.len());
        let mut lines = self.head;
        if omitted_lines > 0 {
            lines.push(FileDiffLine {
                kind: FileDiffLineKind::Omitted(Some(omitted_lines)),
                text: String::new(),
            });
        }
        lines.extend(self.tail);
        FileDiffPreview {
            lines,
            added_lines: self.added_lines,
            removed_lines: self.removed_lines,
            omitted_lines,
        }
    }
}

/// Cuts a line to the preview's own budget, on a character boundary.
fn bounded_line(line: &str) -> String {
    if line.len() <= FILE_DIFF_MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut end = FILE_DIFF_MAX_LINE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{FileOperationKind, FileOperationPatch, FileOperationPatchFormat};

    fn operation(old: Option<&str>, new: Option<&str>) -> FileOperationPayload {
        FileOperationPayload {
            operation: FileOperationKind::Edit,
            path: "src/lib.rs".to_string(),
            summary: "Updated src/lib.rs".to_string(),
            old_text: old.map(str::to_string),
            new_text: new.map(str::to_string),
            patch: None,
            raw_extension: None,
        }
    }

    fn patched(patch: &str) -> FileOperationPayload {
        FileOperationPayload {
            patch: Some(FileOperationPatch {
                format: FileOperationPatchFormat::UnifiedDiffV1,
                text: patch.to_string(),
            }),
            ..operation(None, None)
        }
    }

    #[test]
    fn snapshots_are_projected_through_the_patch_they_synthesize() {
        let preview = project(&operation(Some("one\ntwo\n"), Some("one\nthree\n")))
            .expect("a changed file has a preview");
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 1);
        assert!(
            preview
                .lines
                .iter()
                .any(|line| line.kind == FileDiffLineKind::Added && line.text == "three")
        );
        assert!(
            preview
                .lines
                .iter()
                .any(|line| line.kind == FileDiffLineKind::Removed && line.text == "two")
        );
    }

    #[test]
    fn an_unchanged_file_has_no_preview_at_all() {
        assert!(project(&operation(Some("same\n"), Some("same\n"))).is_none());
    }

    #[test]
    fn a_created_file_reads_as_one_sided_addition() {
        let preview =
            project(&operation(Some(""), Some("alpha\nbeta\n"))).expect("a new file has a preview");
        assert_eq!(preview.added_lines, 2);
        assert_eq!(preview.removed_lines, 0);
    }

    #[test]
    fn counts_come_from_the_patch_without_building_a_preview() {
        let changed = patched("--- old\n+++ new\n@@ -1,2 +1,2 @@\n keep\n-gone\n+here\n");
        assert_eq!(line_counts(&changed), Some((1, 1)));
        // A read, or a patch with no change in it, has nothing to count.
        assert_eq!(line_counts(&operation(None, None)), None);
    }

    #[test]
    fn a_huge_change_keeps_its_head_and_tail_and_says_what_it_dropped() {
        let old = (0..5_000)
            .map(|index| format!("old {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (0..5_000)
            .map(|index| format!("new {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = project(&operation(Some(&old), Some(&new))).expect("a preview");
        assert!(preview.omitted_lines > 0);
        assert!(preview.lines.len() <= FILE_DIFF_PREVIEW_LINES + 1);
        // The counts describe the whole change, not just the kept part.
        assert_eq!(preview.added_lines + preview.removed_lines, 10_000);
        // The dropped run is reported with how much went missing.
        assert!(
            preview.lines.iter().any(|line| {
                line.kind == FileDiffLineKind::Omitted(Some(preview.omitted_lines))
            })
        );
    }

    #[test]
    fn a_single_enormous_line_is_cut_rather_than_laid_out_whole() {
        let long = "x".repeat(FILE_DIFF_MAX_LINE_BYTES * 2);
        let preview = project(&operation(Some(""), Some(&long))).expect("a preview");
        let line = &preview.lines[0];
        assert!(line.text.len() <= FILE_DIFF_MAX_LINE_BYTES + 4);
        assert!(line.text.ends_with('…'));
    }
}
