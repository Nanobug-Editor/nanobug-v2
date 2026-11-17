//! Decode ASCII-compatible sourced scripts with successive :scriptencoding directives.

use super::split_word;
use encoding_rs::{BIG5, EUC_KR, Encoding, GBK, SHIFT_JIS, UTF_8};
use std::io;

/// Decode physical source lines to UTF-8 for the Rust Ex parser.
pub fn decode_ex_source(input: &[u8]) -> io::Result<String> {
    let input = input.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(input);
    let mut encoding = UTF_8;
    let mut source = String::with_capacity(input.len());
    for (index, line) in input.split_inclusive(|&byte| byte == b'\n').enumerate() {
        let text = encoding
            .decode_without_bom_handling_and_without_replacement(line)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {} is invalid {}", index + 1, encoding.name()),
                )
            })?;
        if let Some(name) = source_encoding(text.trim_end()) {
            encoding = match name.to_ascii_lowercase().as_str() {
                "cp932" => SHIFT_JIS,
                "cp936" => GBK,
                "cp949" => EUC_KR,
                "cp950" => BIG5,
                _ => Encoding::for_label(name.as_bytes())
                    .filter(|encoding| encoding.is_ascii_compatible())
                    .unwrap_or(UTF_8),
            };
        }
        source.push_str(&text);
    }
    Ok(source)
}

/// Return the optional source-text encoding requested by an Ex line.
pub(super) fn source_encoding(line: &str) -> Option<&str> {
    let (name, encoding) = split_word(line);
    (name.len() >= 7 && "scriptencoding".starts_with(name)).then_some(encoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source can change encoding and then return to its default UTF-8.
    #[test]
    fn sourced_script_decodes_each_declared_segment() {
        let input = b"scriptencoding cp1252\necho 'caf\xe9'\nscriptencoding\necho '\xe4\xb8\xad'\n";
        assert_eq!(
            decode_ex_source(input).unwrap(),
            "scriptencoding cp1252\necho 'café'\nscriptencoding\necho '中'\n"
        );
    }
}
