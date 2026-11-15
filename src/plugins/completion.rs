//! Case-insensitive subsequence matching shared by language completion and its menu.

use std::ops::Range;

/// Recognize identifier characters shared by generic completion providers.
pub fn identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Find the UTF-8 ranges in `candidate` matching successive characters of `query`.
pub fn matched_chars(query: &str, candidate: &str) -> Option<Vec<Range<usize>>> {
    let mut chars = candidate.char_indices();
    query
        .chars()
        .map(|wanted| {
            let (index, ch) = chars.find(|&(_, ch)| wanted.to_lowercase().eq(ch.to_lowercase()))?;
            Some(index..index + ch.len_utf8())
        })
        .collect()
}

/// Prefer exact prefixes, case-folded prefixes, contiguous matches and then compact subsequences.
pub fn rank(query: &str, candidate: &str) -> Option<(u8, usize, usize)> {
    if candidate.starts_with(query) {
        return Some((0, 0, 0));
    }
    let ranges = matched_chars(query, candidate)?;
    let first = ranges.first()?.start;
    let last = ranges.last()?.end;
    let gaps = last - first - ranges.iter().map(Range::len).sum::<usize>();
    let class = match (first, gaps) {
        (0, 0) => 1,
        (_, 0) => 2,
        _ => 3,
    };
    Some((class, gaps, first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranges_are_ordered_utf8_bytes_and_ignore_unmatched_text() {
        assert_eq!(
            matched_chars("pbf", "PathBuf"),
            Some(vec![0..1, 4..5, 6..7])
        );
        assert_eq!(matched_chars("é目", "É符目"), Some(vec![0..2, 5..8]));
        assert_eq!(matched_chars("fbp", "PathBuf"), None);
        assert!(rank("path", "PathBuf") < rank("path", "module_path!"));
    }
}
