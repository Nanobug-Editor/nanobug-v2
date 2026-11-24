//! Keep Cargo reports that arrive before the affected file is opened in the editor.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

const CACHE_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn from_compiler(item: &Value) -> bool {
    matches!(item["source"].as_str(), Some("rustc" | "clippy"))
}

impl Client {
    /// Accept disk results again when rust-analyzer starts a fresh Cargo invocation.
    pub(super) fn flycheck_progress(&mut self, params: &Value) {
        let Some(token) = params["token"]
            .as_str()
            .filter(|token| token.starts_with("rust-analyzer/flycheck/"))
        else {
            return;
        };
        match params["value"]["kind"].as_str() {
            Some("begin") => {
                self.flychecks.insert(token.to_owned());
                for rust in self
                    .documents
                    .values_mut()
                    .filter_map(|open| open.rust_diagnostics.as_mut())
                {
                    rust.awaiting_check = false;
                }
            }
            Some("end") => {
                self.flychecks.remove(token);
            }
            _ => {}
        }
    }

    /// A project-wide check can finish before this file is first opened.
    pub(super) fn cache_closed_diagnostics(&mut self, uri: &str, mut items: Vec<Value>) {
        items.retain(from_compiler);
        let baseline = || {
            let root = uri_path(self.workspace[0]["uri"].as_str()?)?
                .canonicalize()
                .ok()?;
            let path = uri_path(uri)?.canonicalize().ok()?;
            if !path.starts_with(root) || self.exclusions.contains(&path) {
                return None;
            }
            disk_text(&path)
        };
        let text = if items.is_empty() { None } else { baseline() };
        self.closed_reports.remember(uri, text.as_deref(), &items);
    }
}

/// Bound both the number of saved files and the total serialized diagnostic payload.
#[derive(Default)]
pub(super) struct ClosedReports(VecDeque<SavedReport>);

struct SavedReport {
    uri: String,
    digest: [u8; 32],
    items: Vec<u8>,
}

impl ClosedReports {
    pub(super) fn remember(&mut self, uri: &str, text: Option<&str>, items: &[Value]) {
        self.0.retain(|report| report.uri != uri);
        let Some(text) = text.filter(|_| !items.is_empty()) else {
            return;
        };
        let Ok(items) = serde_json::to_vec(items) else {
            return;
        };
        if items.len() > CACHE_BYTES {
            return;
        }
        let mut bytes = items.len() + self.0.iter().map(|r| r.items.len()).sum::<usize>();
        while self.0.len() >= 64 || bytes > CACHE_BYTES {
            let Some(oldest) = self.0.pop_front() else {
                break;
            };
            bytes -= oldest.items.len();
        }
        self.0.push_back(SavedReport {
            uri: uri.to_owned(),
            digest: Sha256::digest(text.as_bytes()).into(),
            items,
        });
    }

    /// Positions from an older file or a changed external file cannot be reused for this opening.
    pub(super) fn take(&mut self, uri: &str, text: Option<&str>) -> Vec<Value> {
        let Some(index) = self.0.iter().position(|r| r.uri == uri) else {
            return Vec::new();
        };
        let report = self.0.remove(index).expect("found report");
        if text.is_some_and(|s| Sha256::digest(s.as_bytes())[..] == report.digest) {
            serde_json::from_slice(&report.items).unwrap_or_default()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_reports_survive_pulls_edits_and_reopening_only_for_matching_text() -> Result<()> {
        use std::cell::RefCell;
        let project = tempfile::tempdir()?;
        let path = project.path().join("main.rs");
        std::fs::write(&path, "\u{feff}fn mai() {}\r\n")?;
        let uri = file_uri(&path).unwrap();
        let root = file_uri(project.path()).unwrap();
        let doc = |revision, text: &str| {
            Arc::new(DocumentSnapshot {
                opening: 1,
                revision,
                path: Some(path.clone()),
                text: text.into(),
            })
        };
        let (send, _receive) = mpsc::sync_channel(32);
        let mut client = Client::new(
            Server::RustAnalyzer,
            send,
            json!([{"uri":root}]),
            Value::Null,
        );
        client.pull_diagnostics = true;
        let last = RefCell::new(Vec::new());
        let event = |e| {
            if let Event::Diagnostics { items, .. } = e {
                *last.borrow_mut() = items
            }
        };
        let issue = json!({"range":{"start":{"line":0,"character":11},"end":{"line":0,"character":11}},"source":"rustc","severity":1,"code":"E0601"});
        let push = |version: Value, diagnostics: Vec<Value>| json!({"method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":version,"diagnostics":diagnostics}});
        client.on_message(push(Value::Null, vec![issue.clone()]), &event)?;
        let saved = "fn mai() {}\n";
        let fixed = "fn main() {}\n";
        for (revision, text, expected) in [(0, saved, true), (1, fixed, false), (2, saved, true)] {
            client.sync(project.path(), vec![doc(revision, text)])?;
            client
                .documents
                .get_mut(&uri)
                .unwrap()
                .report_diagnostics(Vec::new(), &event);
            assert_eq!(last.borrow().contains(&issue), expected);
        }
        client.sync(project.path(), vec![])?;
        std::fs::write(&path, fixed)?;
        client.sync(project.path(), vec![doc(3, fixed)])?;
        client
            .documents
            .get_mut(&uri)
            .unwrap()
            .report_diagnostics(Vec::new(), &event);
        assert!(last.borrow().is_empty());
        Ok(())
    }
}
