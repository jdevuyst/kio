//! Source-position conversion for the LSP layer.
//!
//! Kio's parser tracks **byte offsets** (`crate::span::Span` is a pair
//! of `u32`s into the original source string). LSP `Position`s, in
//! contrast, are `(line, character)` pairs where `character` defaults
//! to **UTF-16 code units** (per the LSP specification's
//! `positionEncodings`; v1 of `kio lsp` advertises only the default,
//! so every position the client sees is UTF-16-keyed).
//!
//! The conversion needs a per-document index of line starts and
//! UTF-16 prefix lengths so a byte offset can be turned into a
//! `(line, utf16_col)` pair in O(log n) lookups instead of O(n)
//! per-call walks. [`LineIndex`] holds that index and the conversion
//! helpers; the LSP server builds one per analyzed source file and
//! threads it into the diagnostic-mapping pass.
//!
//! Reused by later LSP sessions (hover, goto-definition, completion)
//! that need to map source spans to client positions.

use crate::span::Span;

/// Per-line index over a single source string. For each line, records
/// the byte offset of the line's first character. Conversion from a
/// byte offset to an LSP position walks the prefix of the line in
/// UTF-16 code units, which is bounded by the line length (not the
/// whole file).
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// The source the index is built against. The conversion needs
    /// to re-slice to walk UTF-16 prefix lengths within a line, so
    /// the index owns a copy of the source rather than borrowing it
    /// (the LSP server's source-map handing means the source string
    /// outlives the index, but owning keeps the API monomorphic).
    source: String,
    /// Byte offset of each line's first character. Always begins
    /// with `0` (line 0 starts at byte 0); the last entry is
    /// `source.len()` if the source ends with a newline, otherwise
    /// the offset of the final partial line.
    line_starts: Vec<u32>,
}

impl LineIndex {
    /// Build the line index for `source`. O(n) — one pass to find
    /// every `'\n'`. The byte after each `'\n'` is the start of the
    /// next line; the first line always starts at 0.
    pub fn new(source: &str) -> Self {
        let mut line_starts = Vec::with_capacity(source.len() / 32 + 1);
        line_starts.push(0u32);
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                // The next line starts at byte `i + 1`. Cast is
                // safe: source length is at most u32::MAX in
                // `Span`'s contract; if it ever exceeds, the
                // earlier parse would already have rejected.
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            source: source.to_owned(),
            line_starts,
        }
    }

    /// Number of lines in the source. Equals `line_starts.len()` —
    /// a source ending in `\n` has one extra empty line at the end.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The source text the index was built against.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Convert a byte offset to an LSP `(line, utf16_col)` position.
    /// Both are 0-based. An offset past the end of the source clamps
    /// to the last line's end-of-line position.
    pub fn to_position(&self, byte_offset: u32) -> LspPosition {
        let offset = (byte_offset as usize).min(self.source.len());
        // Binary search for the line containing `offset`.
        // partition_point returns the count of entries strictly less
        // than or equal to offset; `line` is one less.
        let line = self
            .line_starts
            .partition_point(|&start| (start as usize) <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line] as usize;
        let prefix = &self.source[line_start..offset];
        // UTF-16 code units: each `char` contributes 1 if it fits in
        // the BMP, 2 if it's a supplementary character (surrogate
        // pair).
        let utf16_col: usize = prefix.chars().map(|c| c.len_utf16()).sum();
        LspPosition {
            line: line as u32,
            character: utf16_col as u32,
        }
    }

    /// Convert a [`Span`] to a `(start, end)` pair of LSP positions.
    /// The result is the LSP `Range` the diagnostic-mapping layer
    /// hands to `Diagnostic::range`.
    pub fn to_range(&self, span: Span) -> LspRange {
        LspRange {
            start: self.to_position(span.start),
            end: self.to_position(span.end),
        }
    }

    /// Convert an LSP `(line, utf16_col)` position to a byte offset
    /// into the source string. Inverse of [`Self::to_position`]: walks
    /// the target line one character at a time, summing UTF-16 code
    /// units until it reaches `utf16_col`, then returns the byte
    /// offset at that point.
    ///
    /// Out-of-range positions clamp:
    ///
    /// - A `line` past the last line clamps to the source end.
    /// - A `utf16_col` past the line's end clamps to the line's end.
    /// - A `utf16_col` landing inside a surrogate pair (i.e. between
    ///   the two UTF-16 code units of a supplementary character)
    ///   clamps to the start of that character. LSP positions
    ///   shouldn't fall there in practice — editors snap to code-point
    ///   boundaries — but the helper handles the case rather than
    ///   panic.
    ///
    /// Used by the LSP overlay layer to translate `didChange` event
    /// ranges (LSP positions) into byte offsets the splice routine
    /// can act on.
    pub fn position_to_offset(&self, pos: LspPosition) -> u32 {
        let line = pos.line as usize;
        if line >= self.line_starts.len() {
            return self.source.len() as u32;
        }
        let line_start = self.line_starts[line] as usize;
        let line_end = if line + 1 < self.line_starts.len() {
            // Exclude the trailing `\n` from the line slice — the
            // newline byte itself isn't a column within the line.
            self.line_starts[line + 1] as usize - 1
        } else {
            self.source.len()
        };
        let line_slice = &self.source[line_start..line_end];
        let target = pos.character as usize;
        let mut utf16_seen: usize = 0;
        let mut byte_in_line: usize = 0;
        for ch in line_slice.chars() {
            if utf16_seen >= target {
                break;
            }
            let cu = ch.len_utf16();
            // If `target` lands inside a surrogate pair (target ==
            // utf16_seen + 1, cu == 2), stop at the start of the
            // pair — we can't address half of a code point. Snapping
            // to the start is consistent with editors' behavior.
            if utf16_seen + cu > target {
                break;
            }
            utf16_seen += cu;
            byte_in_line += ch.len_utf8();
        }
        (line_start + byte_in_line) as u32
    }
}

/// A `(line, character)` pair in LSP form (both 0-based, `character`
/// counted in UTF-16 code units). Mirrors `lsp_types::Position` so
/// the conversion helper here is independent of the lsp-types crate
/// (the `lsp::diagnostics` module does the final conversion when it
/// builds the `Diagnostic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// A pair of [`LspPosition`]s — start and end — describing a
/// source range. Mirrors `lsp_types::Range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_zero_offset_zero() {
        let idx = LineIndex::new("hello\nworld");
        assert_eq!(
            idx.to_position(0),
            LspPosition {
                line: 0,
                character: 0
            }
        );
    }

    #[test]
    fn mid_first_line() {
        let idx = LineIndex::new("hello\nworld");
        // offset 3 = 'l' in "hello". 3 UTF-16 code units in.
        assert_eq!(
            idx.to_position(3),
            LspPosition {
                line: 0,
                character: 3
            }
        );
    }

    #[test]
    fn start_of_second_line() {
        let idx = LineIndex::new("hello\nworld");
        // offset 6 = 'w' in "world". Line 1, character 0.
        assert_eq!(
            idx.to_position(6),
            LspPosition {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn end_of_source() {
        let idx = LineIndex::new("hello\nworld");
        // offset 11 = past 'd'. Line 1, character 5.
        assert_eq!(
            idx.to_position(11),
            LspPosition {
                line: 1,
                character: 5
            }
        );
    }

    #[test]
    fn offset_past_end_clamps() {
        let idx = LineIndex::new("hello\nworld");
        assert_eq!(
            idx.to_position(1000),
            LspPosition {
                line: 1,
                character: 5
            }
        );
    }

    #[test]
    fn empty_source() {
        let idx = LineIndex::new("");
        assert_eq!(
            idx.to_position(0),
            LspPosition {
                line: 0,
                character: 0
            }
        );
    }

    #[test]
    fn single_newline() {
        let idx = LineIndex::new("\n");
        // Line 0 ends at byte 0 (before the newline); the newline
        // is at byte 0; the second line starts at byte 1.
        assert_eq!(
            idx.to_position(0),
            LspPosition {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            idx.to_position(1),
            LspPosition {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn utf16_basic_multibyte() {
        // `é` is two bytes in UTF-8 and one UTF-16 code unit.
        let src = "héllo";
        let idx = LineIndex::new(src);
        // offset 3 = byte after "hé" (h=1 byte, é=2 bytes) = position
        // at 'l'. 2 UTF-16 code units (h, é).
        assert_eq!(
            idx.to_position(3),
            LspPosition {
                line: 0,
                character: 2
            }
        );
    }

    #[test]
    fn utf16_supplementary_pair() {
        // U+1F600 (grinning face emoji) is 4 bytes in UTF-8 and a
        // surrogate pair (two UTF-16 code units) in UTF-16. After
        // it, the next character is at UTF-16 column 2.
        let src = "\u{1F600}x";
        let idx = LineIndex::new(src);
        // offset 4 = byte after emoji = position at 'x'.
        // 2 UTF-16 code units (the surrogate pair).
        assert_eq!(
            idx.to_position(4),
            LspPosition {
                line: 0,
                character: 2
            }
        );
    }

    #[test]
    fn range_conversion() {
        let idx = LineIndex::new("hello\nworld");
        let span = Span::new(6, 11); // "world"
        let range = idx.to_range(span);
        assert_eq!(
            range.start,
            LspPosition {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            range.end,
            LspPosition {
                line: 1,
                character: 5
            }
        );
    }

    #[test]
    fn line_count_with_trailing_newline() {
        // "a\nb\n" — three line_starts: 0, 2, 4. line 2 is empty.
        let idx = LineIndex::new("a\nb\n");
        assert_eq!(idx.line_count(), 3);
    }

    #[test]
    fn line_count_without_trailing_newline() {
        // "a\nb" — two line_starts: 0, 2. line 1 is "b" (no
        // trailing newline).
        let idx = LineIndex::new("a\nb");
        assert_eq!(idx.line_count(), 2);
    }

    #[test]
    fn position_to_offset_round_trips_first_line() {
        let idx = LineIndex::new("hello\nworld");
        let p = LspPosition {
            line: 0,
            character: 3,
        };
        assert_eq!(idx.position_to_offset(p), 3);
    }

    #[test]
    fn position_to_offset_round_trips_second_line() {
        let idx = LineIndex::new("hello\nworld");
        // line 1, col 0 = byte 6 ('w').
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 1,
                character: 0
            }),
            6
        );
        // line 1, col 5 = byte 11 (past 'd').
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 1,
                character: 5
            }),
            11
        );
    }

    #[test]
    fn position_to_offset_handles_multibyte_utf8() {
        // "héllo": h(1B/1cu) é(2B/1cu) l(1B/1cu) l(1B/1cu) o(1B/1cu).
        // utf16_col=2 (after `hé`) = byte 3 (start of first `l`).
        let idx = LineIndex::new("héllo");
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 2
            }),
            3
        );
    }

    #[test]
    fn position_to_offset_handles_supplementary_pair() {
        // U+1F600 = 4B in UTF-8, 2 UTF-16 code units (surrogate pair).
        // utf16_col=2 = position after the emoji = byte 4.
        let idx = LineIndex::new("\u{1F600}x");
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 2
            }),
            4
        );
    }

    #[test]
    fn position_to_offset_inside_surrogate_pair_clamps() {
        // utf16_col=1 lands in the middle of the surrogate pair.
        // We clamp to the start of the character (byte 0).
        let idx = LineIndex::new("\u{1F600}x");
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 1
            }),
            0
        );
    }

    #[test]
    fn position_to_offset_past_end_clamps() {
        let idx = LineIndex::new("hello");
        // Way past last column on last line.
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 100
            }),
            5
        );
        // Way past last line.
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 100,
                character: 0
            }),
            5
        );
    }

    #[test]
    fn position_to_offset_at_eol_excludes_newline() {
        let idx = LineIndex::new("ab\ncd");
        // line 0, col 5 (past 'b'): clamp to byte 2 (before the '\n').
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 5
            }),
            2
        );
    }

    #[test]
    fn position_to_offset_empty_source() {
        let idx = LineIndex::new("");
        assert_eq!(
            idx.position_to_offset(LspPosition {
                line: 0,
                character: 0
            }),
            0
        );
    }

    #[test]
    fn position_to_offset_round_trips_each_offset() {
        // Sanity: for every byte boundary in the source, converting
        // the byte to a position and back gives the same byte.
        let src = "hello\nworld\nfoo bar";
        let idx = LineIndex::new(src);
        for (byte, _) in src.char_indices() {
            let pos = idx.to_position(byte as u32);
            let back = idx.position_to_offset(pos);
            assert_eq!(back, byte as u32, "round trip failed at byte {byte}");
        }
        // And one past the end.
        let pos = idx.to_position(src.len() as u32);
        assert_eq!(idx.position_to_offset(pos), src.len() as u32);
    }
}
