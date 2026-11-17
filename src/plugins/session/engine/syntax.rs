//! Recoverable syntax worker, separate from plugin execution and installation.
use super::*;
#[derive(Default)]
pub(super) struct Syntax {
    pub(super) worker: Option<Worker>,
    pub(super) restart: Backoff,
}

impl Syntax {
    pub(super) fn parse(
        &mut self,
        id: u64,
        language: String,
        document: nanobug_plugin_sdk::DocumentSnapshot,
        query: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(language.len() <= 32, "Invalid grammar name");
        self.request(id, document, |document| match query {
            Some(query) => Request::QuerySyntax {
                id,
                language,
                query,
                document,
            },
            None => Request::Parse {
                id,
                language,
                document,
            },
        })
    }

    /// Read named node ranges for a native editor operation on `document` in `language`.
    pub(super) fn node_ranges(
        &mut self,
        id: u64,
        language: &str,
        document: DocumentSnapshot,
    ) -> anyhow::Result<Vec<std::ops::Range<usize>>> {
        let value = self.request(id, document, |document| Request::SyntaxNodes {
            id,
            language: language.into(),
            document,
        })?;
        let ranges: Vec<nanobug_plugin_sdk::language::ByteRange> = serde_json::from_value(value)?;
        Ok(ranges
            .into_iter()
            .map(|range| range.start..range.end)
            .collect())
    }

    /// Send a syntax request with identifier `id`, constructing it from pathless `document` bytes.
    fn request(
        &mut self,
        id: u64,
        mut document: DocumentSnapshot,
        request: impl FnOnce(Box<DocumentSnapshot>) -> Request,
    ) -> anyhow::Result<serde_json::Value> {
        // Parsing only needs bytes and version identity. Keep paths out of the worker message.
        document.path = None;
        anyhow::ensure!(
            self.restart.ready(Instant::now()),
            "Syntax worker is recovering"
        );
        if self.worker.is_none() {
            match Worker::start("syntax", None) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.restart.failed(Instant::now());
                    return Err(error);
                }
            }
        }
        let worker = self.worker.as_ref().expect("syntax worker started");
        let document = Box::new(document);
        let response = worker
            .post(request(document))
            .and_then(|()| worker.wait(Duration::from_secs(2)));
        match response {
            Ok(Reply::Parsed { id: actual, value }) if actual == id => Ok(value),
            Ok(Reply::Error {
                id: actual,
                message,
                ..
            }) if actual == id => Err(anyhow::anyhow!(message)),
            _ => {
                self.worker.take();
                self.restart.failed(Instant::now());
                anyhow::bail!("Syntax worker stopped");
            }
        }
    }
}
