//! Decode full relative semantic tokens against the exact UTF-16 overlay advertised to LSP.
use super::*;
use nanobug_plugin_sdk::language::{ByteRange, Highlight, SyntaxKind, SyntaxModifiers};

pub(super) const TYPES: &[&str] = &[
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator",
    "label",
];
pub(super) const MODIFIERS: &[&str] = &["readonly", "static", "deprecated", "defaultLibrary"];

/// Pretranslated server legend and bounded full-document requests.
#[derive(Default)]
pub(super) struct Tokens {
    kinds: Vec<Option<SyntaxKind>>,
    modifiers: Vec<u8>,
    requests: BTreeMap<String, DiagnosticRequest>,
    serial: u64,
}

impl Tokens {
    pub(super) fn configure(&mut self, provider: &Value) {
        use SyntaxKind::*;
        if !advertised(&provider["full"]) {
            return;
        }
        let Some(types) = provider["legend"]["tokenTypes"].as_array() else {
            return;
        };
        self.kinds = types
            .iter()
            .map(|v| {
                Some(match v.as_str()? {
                    "namespace" => Module,
                    "class" => Class,
                    "type" => Type,
                    "struct" => Struct,
                    "enum" => Enum,
                    "interface" | "trait" => Interface,
                    "typeParameter" => TypeParameter,
                    "parameter" => Parameter,
                    "variable" => Variable,
                    "property" => Field,
                    "enumMember" => EnumMember,
                    "function" => Function,
                    "method" => Method,
                    "macro" => Macro,
                    "keyword" => Keyword,
                    "modifier" => Modifier,
                    "comment" => Comment,
                    "string" => String,
                    "number" => Number,
                    "regexp" => Regexp,
                    "operator" => Operator,
                    "decorator" => Attribute,
                    "label" => Label,
                    "lifetime" => Lifetime,
                    "builtinType" => BuiltinType,
                    "const" | "constant" => Constant,
                    _ => return None,
                })
            })
            .collect();
        self.modifiers = provider["legend"]["tokenModifiers"]
            .as_array()
            .into_iter()
            .flatten()
            .take(32)
            .map(|v| match v.as_str() {
                Some("readonly") => 1,
                Some("static") => 2,
                Some("deprecated") => 4,
                Some("defaultLibrary") => 8,
                _ => 0,
            })
            .collect();
    }

    fn decode(&self, text: &str, data: &[Value]) -> Option<Vec<Highlight>> {
        if !data.len().is_multiple_of(5) {
            return None;
        }
        let mut lines = text.split_inclusive('\n');
        let mut base = 0usize;
        let mut line = lines.next().unwrap_or("");
        let (mut column, mut cursor_column, mut cursor) = (0usize, 0usize, 0usize);
        let mut spans = Vec::new();
        for token in data.chunks_exact(5).take(8192) {
            let unsigned = |i: usize| usize::try_from(token[i].as_u64()?).ok();
            let delta = unsigned(0)?;
            for _ in 0..delta {
                base = base.checked_add(line.len())?;
                line = lines.next()?;
            }
            if delta > 0 {
                column = 0;
                cursor = 0;
                cursor_column = 0
            }
            column = column.checked_add(unsigned(1)?)?;
            let end = column.checked_add(unsigned(2)?)?;
            let line_text = line.trim_end_matches(['\r', '\n']);
            let mut advance = |target| {
                if target < cursor_column {
                    return None;
                }
                for ch in line_text[cursor..].chars() {
                    if cursor_column >= target {
                        break;
                    }
                    cursor_column += ch.len_utf16();
                    cursor += ch.len_utf8();
                }
                (cursor_column == target).then_some(base + cursor)
            };
            let start = advance(column)?;
            let finish = advance(end)?;
            let Some(mut kind) = self.kinds.get(unsigned(3)?).copied().flatten() else {
                continue;
            };
            let bits = unsigned(4)?;
            let flags = self
                .modifiers
                .iter()
                .enumerate()
                .filter(|(bit, _)| bits & (1usize << bit) != 0)
                .fold(0, |flags, (_, flag)| flags | flag);
            if flags & 8 != 0 {
                kind = match kind {
                    SyntaxKind::Type => SyntaxKind::BuiltinType,
                    SyntaxKind::Function => SyntaxKind::BuiltinFunction,
                    _ => kind,
                };
            }
            if start < finish {
                spans.push(Highlight {
                    range: ByteRange { start, end: finish },
                    kind,
                    modifiers: SyntaxModifiers(flags & 7),
                });
            }
        }
        Some(spans)
    }
}

impl Client {
    /// Coalesce edits and bound in-flight semantic requests separately from diagnostics.
    pub(super) fn request_semantic_tokens(&mut self, now: Instant) -> Result<()> {
        if self.semantic.kinds.is_empty() {
            return Ok(());
        }
        let expired: Vec<_> = self
            .semantic
            .requests
            .iter()
            .filter(|(_, r)| {
                r.deadline <= now
                    || self
                        .documents
                        .get(&r.uri)
                        .is_none_or(|o| o.version != r.version)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.semantic.requests.remove(&id);
            self.notify("$/cancelRequest", json!({"id":id}))?;
        }
        let slots = 2usize.saturating_sub(self.semantic.requests.len());
        let due: Vec<_> = self
            .documents
            .iter_mut()
            .filter_map(|(uri, open)| {
                if open.semantic_at? > now {
                    return None;
                }
                open.semantic_at = None;
                Some((uri.clone(), open.version))
            })
            .take(slots)
            .collect();
        for (uri, version) in due {
            self.semantic.serial += 1;
            let id = format!("semantic-{}", self.semantic.serial);
            self.send(json!({"jsonrpc":"2.0","id":id,"method":"textDocument/semanticTokens/full","params":{"textDocument":{"uri":uri}}}))?;
            self.semantic.requests.insert(
                id,
                DiagnosticRequest {
                    uri,
                    version,
                    deadline: now + Duration::from_secs(10),
                },
            );
        }
        Ok(())
    }

    /// Drop expired, malformed or obsolete replies before publishing to the workspace.
    pub(super) fn semantic_response(
        &mut self,
        id: &str,
        value: Option<&Value>,
        event: &impl Fn(Event),
    ) {
        let Some(r) = self.semantic.requests.remove(id) else {
            return;
        };
        let Some(open) = self
            .documents
            .get(&r.uri)
            .filter(|o| o.version == r.version && r.deadline > Instant::now())
        else {
            return;
        };
        let spans = match value {
            Some(Value::Null) => Vec::new(),
            Some(value) => {
                let Some(spans) = value["data"]
                    .as_array()
                    .and_then(|data| self.semantic.decode(&open.snapshot.text, data))
                else {
                    return;
                };
                spans
            }
            None => return,
        };
        event(Event::Semantic {
            snapshot: open.snapshot.clone(),
            spans,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_follow_the_server_legend_and_drop_replaced_documents() -> Result<()> {
        use std::cell::RefCell;
        let root = tempfile::tempdir()?;
        let path = root.path().join("lib.rs");
        let source = |revision| {
            Arc::new(DocumentSnapshot {
                opening: 1,
                revision,
                path: Some(path.clone()),
                text: "save".into(),
            })
        };
        let (tx, rx) = mpsc::sync_channel(24);
        let mut client = Client::new(Server::RustAnalyzer, tx, Value::Null, Value::Null);
        client.initialized(&json!({"result":{"capabilities":{"semanticTokensProvider":{"full":true,"legend":{"tokenTypes":["method"],"tokenModifiers":["deprecated"]}}}}}))?;
        let _ = rx.try_recv()?;
        client.sync(root.path(), vec![source(0)])?;
        let _ = rx.try_recv()?;
        client.request_semantic_tokens(Instant::now() + DIAGNOSTIC_IDLE)?;
        let first = rx.try_recv()?;
        assert_eq!(first["method"], "textDocument/semanticTokens/full");
        client.sync(root.path(), vec![source(1)])?;
        let _ = rx.try_recv()?;
        let observed = RefCell::new(Vec::new());
        let publish = |event| {
            if let Event::Semantic { snapshot, spans } = event {
                observed.borrow_mut().push((snapshot.revision, spans));
            }
        };
        let reply = |id: &Value| json!({"jsonrpc":"2.0","id":id,"result":{"data":[0,0,4,0,1]}});
        client.on_message(reply(&first["id"]), &publish)?;
        assert!(observed.borrow().is_empty());
        client.request_semantic_tokens(Instant::now() + DIAGNOSTIC_IDLE)?;
        client.on_message(reply(&rx.try_recv()?["id"]), &publish)?;
        assert_eq!(observed.borrow()[0].0, 1);
        assert_eq!(observed.borrow()[0].1[0].modifiers.0, 4);
        Ok(())
    }

    #[test]
    fn semantic_tokens_keep_modifiers_and_utf8_bounds() {
        let mut t = Tokens::default();
        t.configure(&json!({"full":true,"legend":{"tokenTypes":["method","variable"],"tokenModifiers":["deprecated","static"]}}));
        let data = json!([0, 3, 4, 0, 3, 1, 0, 1, 1, 0]);
        let h = t
            .decode("😀.save()\r\nx", data.as_array().unwrap())
            .unwrap();
        assert_eq!(
            (h[0].range, h[0].kind, h[0].modifiers.0),
            (ByteRange { start: 5, end: 9 }, SyntaxKind::Method, 6)
        );
        assert_eq!((h[1].range.start, h[1].range.end), (13, 14));
        assert!(
            t.decode("😀", json!([0, 1, 1, 0, 0]).as_array().unwrap())
                .is_none()
        );
    }
}
