use super::TermGridPos;
use crate::pty::{SnapshotCell, TerminalSnapshot};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const URL_PREFIXES: [&str; 2] = ["https://", "http://"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalLinkRange {
    pub url: String,
    spans: Vec<TerminalLinkSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalLinkSpan {
    row: u16,
    start_col: u16,
    end_col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PositionedChar {
    ch: char,
    row: u16,
    col: u16,
    width: u16,
}

pub(super) fn ranges(snapshot: &TerminalSnapshot) -> Vec<TerminalLinkRange> {
    logical_lines(snapshot)
        .into_iter()
        .flat_map(|line| ranges_for_line(&line))
        .collect()
}

pub(super) fn url_at_grid_pos(snapshot: &TerminalSnapshot, pos: TermGridPos) -> Option<String> {
    ranges(snapshot)
        .into_iter()
        .find(|range| range.contains(pos))
        .map(|range| range.url)
}

pub(super) fn url_for_cell<'a>(
    cell: &SnapshotCell,
    ranges: &'a [TerminalLinkRange],
) -> Option<&'a str> {
    let cell_width = u16::try_from(cell.symbol.width().max(1)).unwrap_or(1);
    let cell_end = cell.col.saturating_add(cell_width);
    ranges
        .iter()
        .find(|range| {
            range.spans.iter().any(|span| {
                span.row == cell.row && cell.col < span.end_col && cell_end > span.start_col
            })
        })
        .map(|range| range.url.as_str())
}

impl TerminalLinkRange {
    fn contains(&self, pos: TermGridPos) -> bool {
        self.spans
            .iter()
            .any(|span| span.row == pos.row && pos.col >= span.start_col && pos.col < span.end_col)
    }
}

fn logical_lines(snapshot: &TerminalSnapshot) -> Vec<Vec<PositionedChar>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    for row in 0..snapshot.rows {
        current.extend(row_chars(snapshot, row));
        if !snapshot.row_wraps(row) {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn row_chars(snapshot: &TerminalSnapshot, row: u16) -> Vec<PositionedChar> {
    let mut row_cells: Vec<_> = snapshot
        .cells
        .iter()
        .filter(|cell| cell.row == row)
        .collect();
    row_cells.sort_by_key(|cell| cell.col);

    let mut chars = Vec::new();
    let mut next_col = 0u16;
    for cell in row_cells {
        while next_col < cell.col {
            chars.push(PositionedChar {
                ch: ' ',
                row,
                col: next_col,
                width: 1,
            });
            next_col = next_col.saturating_add(1);
        }

        let mut symbol_col = cell.col;
        for ch in cell.symbol.chars() {
            let width = u16::try_from(UnicodeWidthChar::width(ch).unwrap_or(0)).unwrap_or(0);
            chars.push(PositionedChar {
                ch,
                row,
                col: if width == 0 {
                    symbol_col.saturating_sub(1)
                } else {
                    symbol_col
                },
                width,
            });
            symbol_col = symbol_col.saturating_add(width);
        }
        next_col = next_col.max(symbol_col);
    }
    chars
}

fn ranges_for_line(line: &[PositionedChar]) -> Vec<TerminalLinkRange> {
    let chars: Vec<_> = line.iter().map(|positioned| positioned.ch).collect();
    let mut ranges = Vec::new();
    for start in 0..chars.len() {
        if !URL_PREFIXES
            .iter()
            .any(|prefix| chars_start_with(&chars, start, prefix))
        {
            continue;
        }

        let mut end = start;
        while end < chars.len() && !chars[end].is_whitespace() && !chars[end].is_ascii_control() {
            end += 1;
        }
        end = trim_trailing_punctuation(&chars, start, end);
        if end <= start {
            continue;
        }

        ranges.push(TerminalLinkRange {
            url: chars[start..end].iter().collect(),
            spans: spans_for_chars(&line[start..end]),
        });
    }
    ranges
}

fn spans_for_chars(chars: &[PositionedChar]) -> Vec<TerminalLinkSpan> {
    let mut spans: Vec<TerminalLinkSpan> = Vec::new();
    for positioned in chars.iter().filter(|positioned| positioned.width > 0) {
        let end_col = positioned.col.saturating_add(positioned.width);
        if let Some(span) = spans.last_mut()
            && span.row == positioned.row
            && positioned.col <= span.end_col
        {
            span.end_col = span.end_col.max(end_col);
        } else {
            spans.push(TerminalLinkSpan {
                row: positioned.row,
                start_col: positioned.col,
                end_col,
            });
        }
    }
    spans
}

fn trim_trailing_punctuation(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let trailing = chars[end - 1];
        let should_trim = match trailing {
            ')' => unmatched_closing_delimiter(chars, start, end, '(', ')'),
            ']' => unmatched_closing_delimiter(chars, start, end, '[', ']'),
            '}' => unmatched_closing_delimiter(chars, start, end, '{', '}'),
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' => true,
            _ => false,
        };
        if !should_trim {
            break;
        }
        end -= 1;
    }
    end
}

fn unmatched_closing_delimiter(
    chars: &[char],
    start: usize,
    end: usize,
    open: char,
    close: char,
) -> bool {
    let mut depth = 0usize;
    for ch in &chars[start..end] {
        if *ch == open {
            depth = depth.saturating_add(1);
        } else if *ch == close {
            let Some(next_depth) = depth.checked_sub(1) else {
                return true;
            };
            depth = next_depth;
        }
    }
    false
}

fn chars_start_with(chars: &[char], start: usize, prefix: &str) -> bool {
    let prefix_len = prefix.chars().count();
    if start.saturating_add(prefix_len) > chars.len() {
        return false;
    }
    chars[start..start + prefix_len]
        .iter()
        .copied()
        .eq(prefix.chars())
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use ratatui::style::{Color, Modifier};

    fn snapshot(rows: &[&str], wrapped_rows: &[bool]) -> TerminalSnapshot {
        let cols = rows.iter().map(|row| row.width()).max().unwrap_or_default();
        let cells = rows
            .iter()
            .enumerate()
            .flat_map(|(row, text)| {
                let mut col = 0u16;
                text.chars().map(move |ch| {
                    let cell = SnapshotCell {
                        row: u16::try_from(row).unwrap(),
                        col,
                        symbol: CompactString::from(ch.to_string()),
                        fg: Color::Reset,
                        bg: Color::Reset,
                        modifier: Modifier::empty(),
                    };
                    col = col.saturating_add(
                        u16::try_from(UnicodeWidthChar::width(ch).unwrap_or(0)).unwrap_or(0),
                    );
                    cell
                })
            })
            .collect();
        TerminalSnapshot {
            rows: u16::try_from(rows.len()).unwrap(),
            cols: u16::try_from(cols).unwrap(),
            scrollback_offset: 0,
            scrollback_total: 0,
            cursor: None,
            cells,
            wrapped_rows: wrapped_rows.to_vec(),
        }
    }

    #[test]
    fn detects_url_across_soft_wrapped_rows() {
        let snapshot = snapshot(&["See https://example.", "com/path now"], &[true, false]);

        let ranges = ranges(&snapshot);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].url, "https://example.com/path");
        assert_eq!(
            url_at_grid_pos(&snapshot, TermGridPos { row: 1, col: 2 }).as_deref(),
            Some("https://example.com/path")
        );
        let continuation = snapshot
            .cells
            .iter()
            .find(|cell| cell.row == 1 && cell.col == 0)
            .unwrap();
        assert_eq!(
            url_for_cell(continuation, &ranges),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn preserves_balanced_url_delimiters_and_trims_sentence_delimiter() {
        let snapshot = snapshot(
            &["See (https://en.wikipedia.org/wiki/Function_(mathematics))."],
            &[false],
        );

        let ranges = ranges(&snapshot);

        assert_eq!(ranges.len(), 1);
        assert_eq!(
            ranges[0].url,
            "https://en.wikipedia.org/wiki/Function_(mathematics)"
        );
    }

    #[test]
    fn combining_character_before_url_does_not_shift_hit_testing() {
        let mut snapshot = snapshot(&["  https://example.com"], &[false]);
        snapshot.cells[0].symbol = CompactString::from("e\u{301}");

        assert_eq!(
            url_at_grid_pos(&snapshot, TermGridPos { row: 0, col: 2 }).as_deref(),
            Some("https://example.com")
        );
    }
}
