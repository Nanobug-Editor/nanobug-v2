//! Normalize language-server hover contents into bounded Markdown.

use serde_json::Value;

/// Limit the generated Markdown to this many UTF-8 bytes.
const MAX_BYTES: usize = 16 * 1024;
/// Maximum number of sections read from a legacy marked-string array.
const MAX_SECTIONS: usize = 64;

/// Convert `result`'s hover contents to Markdown, preserving each section's declared format.
pub(super) fn details(result: &Value) -> Option<String> {
    let contents = result.get("contents")?;
    let sections = contents
        .as_array()
        .map_or_else(|| std::slice::from_ref(contents), Vec::as_slice);
    let mut details = String::new();
    for section in sections.iter().take(MAX_SECTIONS) {
        let Some(text) = section.as_str().or_else(|| section["value"].as_str()) else {
            continue;
        };
        let separator = if details.is_empty() { "" } else { "\n\n" };
        let Some(budget) = MAX_BYTES.checked_sub(details.len() + separator.len()) else {
            break;
        };
        let text = strip_controls(text, budget);
        if text.trim().is_empty() {
            continue;
        }
        let body = if section["kind"].as_str() == Some("plaintext") {
            escape_plaintext(text.trim(), budget)
        } else if let Some(language) = section["language"].as_str() {
            let Some(code) = fenced_code(text.trim_matches('\n'), language, budget) else {
                continue;
            };
            code
        } else {
            text
        };
        if !body.is_empty() {
            details.push_str(separator);
            details.push_str(&body);
        }
    }
    (!details.is_empty()).then_some(details)
}

/// Copy `text` up to `budget` bytes, retaining only the control characters used in Markdown.
fn strip_controls(text: &str, budget: usize) -> String {
    let mut body = String::new();
    for ch in text
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
    {
        if body.len() + ch.len_utf8() > budget {
            break;
        }
        body.push(ch);
    }
    body
}

/// Quote `text` as ordinary Markdown paragraphs, with literal punctuation and line breaks.
/// Keep output within `budget` UTF-8 bytes and never leave half of an escape at the cutoff.
fn escape_plaintext(text: &str, budget: usize) -> String {
    let mut escaped = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim();
        for ch in line.chars() {
            let needs_escape = ch.is_ascii_punctuation();
            if escaped.len() + ch.len_utf8() + usize::from(needs_escape) > budget {
                return escaped;
            }
            if needs_escape {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        if lines.peek().is_some() {
            let newline = if line.is_empty() { "\n" } else { "  \n" };
            if escaped.len() + newline.len() > budget {
                return escaped;
            }
            escaped.push_str(newline);
        }
    }
    escaped
}

/// Fence a legacy code `text` snippet with its `language` and fit the result within `budget`.
fn fenced_code(text: &str, language: &str, budget: usize) -> Option<String> {
    let language = (language.len() <= 32)
        .then_some(language)
        .filter(|value| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_+-#".contains(&byte))
        })
        .unwrap_or_default();
    let backticks = fence_length(text, '`');
    let fence = if backticks == 3 {
        "```".into()
    } else {
        let tildes = fence_length(text, '~');
        if tildes < backticks {
            "~".repeat(tildes)
        } else {
            "`".repeat(backticks)
        }
    };
    let content_budget = budget.checked_sub(2 * fence.len() + language.len() + 2)?;
    let text = &text[..text.floor_char_boundary(content_budget.min(text.len()))];
    (!text.trim().is_empty()).then(|| format!("{fence}{language}\n{text}\n{fence}"))
}

/// Find the shortest safe fence using `marker` around `text`.
fn fence_length(text: &str, marker: char) -> usize {
    let longest = text
        .split(|ch| ch != marker)
        .map(str::len)
        .max()
        .unwrap_or_default();
    (longest + 1).max(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Handle current markup objects and the legacy section array in presentation order.
    #[test]
    fn hover_details_preserve_signatures_types_and_documents() {
        let markdown = "```rust\nfn twice(value: u32) -> u32\n```\n\nDoubles a value.";
        assert_eq!(
            details(&json!({"contents": {"kind": "markdown", "value": markdown}})).as_deref(),
            Some(markdown)
        );
        assert_eq!(
            details(&json!({"contents": [
                {"language":"rust", "value":"fn twice(value: u32) -> u32"},
                "Doubles a value."
            ]}))
            .as_deref(),
            Some(markdown)
        );
        assert_eq!(
            details(&json!({"contents":"Signed integer."})).as_deref(),
            Some("Signed integer.")
        );
        assert!(details(&Value::Null).is_none());
        assert!(details(&json!({"contents": [null, {}, "\u{0000}\n"]})).is_none());
    }

    /// Keep explicit plaintext literal even when it resembles Markdown or an autolink.
    #[test]
    fn hover_plaintext_keeps_literal_markup_and_line_breaks() {
        use pulldown_cmark::{Event, Parser, Tag, TagEnd};

        let value = json!({"contents": {
            "kind": "plaintext",
            "value": "# Heading\r\n[link](https://example.com) **bold** <https://example.com>\u{0000}",
        }});
        let display = details(&value).expect("hover contents");
        let mut literal = String::new();
        for event in Parser::new(&display) {
            match event {
                Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
                Event::Text(text) => literal.push_str(&text),
                Event::HardBreak => literal.push('\n'),
                other => panic!("unexpected plaintext event: {other:?}"),
            }
        }
        assert_eq!(
            literal,
            "# Heading\n[link](https://example.com) **bold** <https://example.com>"
        );
        assert_eq!(escape_plaintext("🌱**", 7), "🌱\\*");
    }

    /// A run of backticks inside a legacy snippet cannot close its own code fence.
    #[test]
    fn hover_legacy_code_keeps_inner_fences_verbatim() {
        let value = json!({"contents": [
            {"language": "rust", "value": "let docs = r#\"\n```\n\"#;"},
            "The **documentation**."
        ]});
        assert_eq!(
            details(&value).as_deref(),
            Some("~~~rust\nlet docs = r#\"\n```\n\"#;\n~~~\n\nThe **documentation**.")
        );
    }

    /// Truncate on character boundaries and still close an added code fence.
    #[test]
    fn hover_details_bound_large_unicode_sections() {
        let value = json!({"contents": {"language": "rust", "value": "🌱".repeat(MAX_BYTES)}});
        let display = details(&value).expect("hover contents");
        assert!(display.len() <= MAX_BYTES);
        assert!(display.starts_with("```rust\n🌱"));
        assert!(display.ends_with("\n```"));
    }
}
