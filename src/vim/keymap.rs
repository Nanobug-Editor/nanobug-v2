//! Load Vim runtime keymap tables without sourcing executable Vimscript.

use std::{
    borrow::Cow,
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Maximum size of one keymap file, including its metadata and mapping lines.
const LIMIT: u64 = 1024 * 1024;
/// Maximum alias indirection in a catalog.
const ALIAS_LIMIT: usize = 8;

/// Failures distinguished by the Ex command's localized feedback.
#[derive(Debug)]
pub(super) enum Error {
    /// The requested name is not a valid Vim keymap option value.
    Name,
    /// No matching keymap file exists in the searched catalogs.
    Missing,
    /// A selected file or its alias could not be read or decoded.
    File(String),
}

/// Parsed keymap metadata and unexpanded left/right notation.
pub(super) struct Table {
    /// Name supplied via `let b:keymap_name`, when present.
    pub short_name: Option<String>,
    /// A row for each language mapping in written order.
    pub rows: Vec<(String, String)>,
}

/// Load the keymap named `name` from the project `directory` or portable data.
pub(super) fn load(directory: &Path, name: &str) -> Result<Table, Error> {
    if !valid_name(name) {
        return Err(Error::Name);
    }
    let roots = roots(directory);
    for file in [format!("{name}_utf-8.vim"), format!("{name}.vim")] {
        for root in &roots {
            if let Some(bytes) = read_disk(&root.join(&file))? {
                return resolve(&file, &bytes, |alias| read_disk(&root.join(alias)), 0);
            }
        }
    }
    Err(Error::Missing)
}

/// Decode the `loadkeymap` rows of a user-sourced Ex script.
pub(super) fn sourced_keymap(source: &str) -> Result<Table, Error> {
    resolve("keymap", source.as_bytes(), |_| Ok(None), 0)
}

/// Offer keymap names starting with `prefix`, including user files in `directory`.
pub(super) fn candidates(directory: &Path, prefix: &str) -> Vec<String> {
    let mut result = BTreeSet::new();
    let mut add = |filename: &str| {
        if let Some(stem) = filename.strip_suffix(".vim") {
            for name in [stem, stem.strip_suffix("_utf-8").unwrap_or(stem)] {
                if name.starts_with(prefix) && valid_name(name) {
                    result.insert(name.to_owned());
                }
            }
        }
    };
    for root in roots(directory) {
        for entry in fs::read_dir(root).into_iter().flatten().flatten() {
            if let Some(filename) = entry.file_name().to_str() {
                add(filename);
            }
        }
    }
    result.into_iter().collect()
}

/// Return catalogs for the current project `directory` and portable user files.
fn roots(directory: &Path) -> Vec<PathBuf> {
    let mut roots = vec![directory.join("keymap")];
    if let Ok(executable) = env::current_exe()
        && let Ok(data) = crate::app_data::directory_for(&executable)
    {
        roots.push(data.join("keymap"));
    }
    roots
}

/// Validate a Vim option `name` and keep lookup within a keymap catalog.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Read the keymap at `path` within the size limit, or return `None` if it is missing.
fn read_disk(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::File(error.to_string())),
    };
    if file.metadata().is_ok_and(|info| !info.is_file()) {
        return Err(Error::File(path.display().to_string()));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::File(error.to_string()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::File("keymap exceeds 1 MiB".into()));
    }
    Ok(Some(bytes))
}

/// Decode `bytes` as UTF-8, declared scriptencoding, or the encoding suffix of `file`.
fn decode<'a>(file: &str, bytes: &'a [u8]) -> Result<Cow<'a, str>, Error> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(Cow::Borrowed(text));
    }
    let ascii_header = String::from_utf8_lossy(bytes);
    let declared = ascii_header.lines().find_map(|line| {
        let mut words = line.split_ascii_whitespace();
        words
            .next()
            .filter(|command| command.eq_ignore_ascii_case("scriptencoding"))
            .and_then(|_| words.next())
    });
    let encoding = declared.or_else(|| {
        file.strip_suffix(".vim")?
            .rsplit_once('_')
            .map(|(_, encoding)| encoding)
    });
    let Some(encoding) = encoding else {
        return Err(Error::File(
            "expected UTF-8 or a declared scriptencoding".into(),
        ));
    };
    // The cp737 catalog spells all mapped characters numerically. Its legacy bytes are comments.
    if ["latin1", "iso-8859-1", "cp737"]
        .iter()
        .any(|label| encoding.eq_ignore_ascii_case(label))
    {
        return Ok(Cow::Owned(
            bytes.iter().map(|&byte| char::from(byte)).collect(),
        ));
    }
    let label = encoding.to_ascii_lowercase();
    let label = label
        .strip_prefix("cp")
        .and_then(|number| number.parse::<u16>().ok())
        .filter(|number| (1250..=1258).contains(number))
        .map_or(label.clone(), |number| format!("windows-{number}"));
    encoding_rs::Encoding::for_label(label.as_bytes())
        .and_then(|encoding| encoding.decode_without_bom_handling_and_without_replacement(bytes))
        .ok_or_else(|| Error::File(format!("cannot decode scriptencoding {encoding}")))
}

/// Parse the `loadkeymap` table in `file` bytes and follow sibling aliases via `read_alias` to `depth`.
fn resolve(
    file: &str,
    bytes: &[u8],
    read_alias: impl Fn(&str) -> Result<Option<Vec<u8>>, Error> + Copy,
    depth: usize,
) -> Result<Table, Error> {
    let text = decode(file, bytes)?;
    let mut short_name = None;
    let mut aliases = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((line, raw)) = lines.next() {
        let raw = raw.trim();
        let command = raw.trim_start_matches(':').split_ascii_whitespace().next();
        if command.is_some_and(|word| word.len() >= 5 && "loadkeymap".starts_with(word)) {
            let rows = lines
                .filter_map(|(line, raw)| {
                    let raw = raw.trim_start_matches([' ', '\t']);
                    (!raw.is_empty() && !raw.starts_with('"')).then_some((line, raw))
                })
                .map(|(line, raw)| mapping_row(raw, line + 1))
                .collect::<Result<_, _>>()?;
            return Ok(Table { short_name, rows });
        }
        if let Some((name, value)) = raw.strip_prefix("let ").and_then(|rhs| rhs.split_once('='))
            && matches!(name.trim(), "b:keymap_name" | "b:keymap")
        {
            let value = value.trim();
            let quote = value.chars().next().unwrap_or_default();
            if matches!(quote, '\'' | '"')
                && let Some(end) = value[1..].find(quote)
            {
                short_name = Some(value[1..end + 1].to_owned());
            }
        } else if let Some(alias) = raw.strip_prefix("source <sfile>:p:h/") {
            let alias = alias.split_ascii_whitespace().next().unwrap_or_default();
            if alias.strip_suffix(".vim").is_some_and(valid_name) {
                aliases.push(alias);
            } else {
                return Err(Error::File(format!(
                    "line {} has an invalid alias",
                    line + 1
                )));
            }
        }
    }
    if depth < ALIAS_LIMIT {
        // Multi-encoding upstream wrappers select the UTF-8 sibling in Nanobug.
        let alias = aliases
            .iter()
            .find(|name| name.ends_with("_utf-8.vim"))
            .or_else(|| aliases.first());
        if let Some(alias) = alias
            && let Some(bytes) = read_alias(alias)?
        {
            return resolve(alias, &bytes, read_alias, depth + 1);
        }
    }
    Err(Error::File(
        "missing loadkeymap table or unresolved alias".into(),
    ))
}

/// Decode the two tokens in `raw` before any trailing comment on `line`.
fn mapping_row(raw: &str, line: usize) -> Result<(String, String), Error> {
    let mut fields = raw.split_ascii_whitespace();
    let left = fields.next().unwrap_or_default();
    let right = fields.next().unwrap_or_default();
    if left.is_empty() || right.is_empty() {
        return Err(Error::File(format!(
            "line {line} requires two mapping fields"
        )));
    }
    Ok((unescape(left), unescape(right)))
}

/// Decode Vim's backslash escapes in a `loadkeymap` mapping field.
fn unescape(field: &str) -> String {
    let mut output = String::new();
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.next()
        {
            if next == '<' {
                output.push_str("<lt>");
            } else {
                output.push(next);
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Complete a project keymap name and follow its alias to a sibling table.
    #[test]
    fn project_keymap_alias_loads_and_completes() {
        let project = tempfile::tempdir().unwrap();
        let keymaps = project.path().join("keymap");
        fs::create_dir(&keymaps).unwrap();
        fs::write(
            keymaps.join("nanobug-test-alias.vim"),
            "source <sfile>:p:h/nanobug-test-target_utf-8.vim\n",
        )
        .unwrap();
        fs::write(
            keymaps.join("nanobug-test-target_utf-8.vim"),
            "let b:keymap_name = 'test'\nloadkeymap\na 日\n",
        )
        .unwrap();
        let table = load(project.path(), "nanobug-test-alias").unwrap();
        assert_eq!(table.short_name.as_deref(), Some("test"));
        assert_eq!(table.rows, [("a".to_owned(), "日".to_owned())]);
        assert_eq!(
            candidates(project.path(), "nanobug-test-a"),
            ["nanobug-test-alias"]
        );
    }
}
