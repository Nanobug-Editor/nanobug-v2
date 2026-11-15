//! Pair Vue's template service with the Vue TypeScript plugin over bounded LSP pipes.

use super::*;

/// Vue's current npm package and its executable entry point.
const VUE_ENTRY: &str = "node_modules/@vue/language-server/bin/vue-language-server.js";
/// Cap Vue-to-TypeScript work independently of the editor's own request queue.
const MAX_BRIDGE_REQUESTS: usize = 24;

/// The secondary Vue endpoint and its pending metadata requests to TypeScript.
pub(super) struct Bridge {
    endpoint: Endpoint,
    client: Client,
    plugin_location: PathBuf,
    initialized: bool,
    deadline: Instant,
    next_id: u64,
    requests: BTreeMap<String, Forwarded>,
}

/// The original Volar notification ID and how long it may wait for TypeScript.
struct Forwarded {
    id: u64,
    deadline: Instant,
}

impl Bridge {
    /// Launch Volar with the same installed package location used to load its TypeScript plugin.
    pub(super) fn start(
        root: &Path,
        root_uri: &str,
        workspace: &Value,
        exclusions: Arc<ScanExclusions>,
    ) -> Result<Self> {
        use super::super::language::{executable, npm_entry};
        let entry = root.join(VUE_ENTRY);
        let entry = entry.is_file().then_some(entry).or_else(|| npm_entry(VUE_ENTRY))
            .context("Install @vue/language-server@3 in the project or in a global npm directory on PATH")?;
        let plugin_location = entry
            .parent()
            .and_then(Path::parent)
            .context("Vue language server package directory")?
            .to_owned();
        let node = executable("node").context("Vue language server needs Node on PATH")?;
        let mut command = super::super::command(&node, root);
        command.arg(entry).arg("--stdio");
        let mut options = Value::Null;
        let mut configuration = Map::new();
        if !exclusions.directories().is_empty() {
            frontend_exclusions::apply(
                Server::Vue,
                root,
                exclusions.directories(),
                &mut command,
                &mut options,
                &mut configuration,
            )?;
        }
        let endpoint = start_endpoint(command, Server::Vue.name())?;
        let mut client = Client::new(
            Server::Vue,
            endpoint.writer.clone(),
            workspace.clone(),
            options,
        );
        client.exclusions = exclusions;
        client.configuration = configuration;
        client.send(initialize(
            root_uri.to_owned(),
            workspace,
            &client.options,
            false,
        ))?;
        Ok(Self {
            endpoint,
            client,
            plugin_location,
            initialized: false,
            deadline: Instant::now() + INITIALIZE_WAIT,
            next_id: 0,
            requests: BTreeMap::new(),
        })
    }

    /// Register the matching Vue plugin before TypeScript opens any Vue overlays.
    pub(super) fn typescript_options(&self) -> Value {
        json!({
            "disableAutomaticTypingAcquisition": true,
            "plugins": [{
                "name": "@vue/typescript-plugin",
                "location": self.plugin_location,
                "languages": ["vue"]
            }]
        })
    }

    /// The main worker waits for both servers to initialize before syncing overlays.
    pub(super) fn ready(&self) -> bool {
        self.initialized
    }

    /// Count editor requests forwarded to the secondary endpoint against the same queue cap.
    pub(super) fn pending(&self) -> usize {
        self.client.pending.len()
    }

    /// Synchronize only Vue buffers to Volar. The paired TypeScript service sees JS and TS too.
    pub(super) fn sync(&mut self, root: &Path, documents: &[Arc<DocumentSnapshot>]) -> Result<()> {
        self.client.sync(
            root,
            documents
                .iter()
                .filter(|doc| {
                    doc.path
                        .as_deref()
                        .and_then(|path| Server::Vue.language_id(path))
                        == Some("vue")
                })
                .cloned()
                .collect(),
        )
    }

    /// Process Vue requests and return the primary worker to its edit loop after a bounded batch.
    pub(super) fn poll(&mut self, primary: &Client, event: &impl Fn(Event)) -> Result<()> {
        let now = Instant::now();
        self.client.expire(now, event)?;
        let expired: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, request)| now >= request.deadline)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let request = self.requests.remove(&id).expect("bridge request exists");
            primary.notify("$/cancelRequest", json!({"id": id}))?;
            self.send_result(request.id, &Value::Null)?;
        }
        for _ in 0..24 {
            match self.endpoint.incoming.try_recv() {
                Ok(Ok(message))
                    if !self.initialized
                        && message["id"] == 0
                        && message.get("method").is_none() =>
                {
                    self.client
                        .initialized(&message)
                        .context("Vue language server initialization")?;
                    self.initialized = true;
                }
                Ok(Ok(message)) if message["method"] == "tsserver/request" => {
                    self.forward_request(primary, &message["params"][0])?;
                }
                // TypeScript owns version-bound semantic diagnostics for Vue buffers.
                Ok(Ok(message)) if message["method"] == "textDocument/publishDiagnostics" => {}
                Ok(Ok(message)) => self.client.on_message(message, event)?,
                Ok(Err(error)) => return Err(error).context("Vue language server stopped"),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("Vue language server output ended")
                }
            }
        }
        ensure!(
            self.initialized || now < self.deadline,
            "Vue language server initialization timed out"
        );
        Ok(())
    }

    /// Route TypeScript's responses back to Vue or fall back to the template service for completion.
    pub(super) fn handle_primary(
        &mut self,
        primary: &mut Client,
        message: &Value,
        event: &impl Fn(Event),
    ) -> Result<bool> {
        if message.get("method").is_some() {
            return Ok(false);
        }
        if let Some(id) = message["id"].as_str()
            && let Some(request) = self.requests.remove(id)
        {
            let body = if Instant::now() < request.deadline {
                &message["result"]["body"]
            } else {
                &Value::Null
            };
            self.send_result(request.id, body)?;
            return Ok(true);
        }
        let Some(id) = message["id"].as_u64() else {
            return Ok(false);
        };
        let result = &message["result"];
        let found = result
            .as_array()
            .or_else(|| result["items"].as_array())
            .is_some_and(|items| !items.is_empty());
        if found || !self.client.completion {
            return Ok(false);
        }
        if primary.pending.get(&id).is_some_and(|query| {
            matches!(query.kind, Request::Completion)
                && primary.current(query).is_some()
                && self.client.current(query).is_some()
                && Instant::now() < query.deadline
        }) {
            let query = primary
                .pending
                .remove(&id)
                .expect("completion request exists");
            self.client.request(query, event)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Forward only Vue's read-only metadata requests through the TypeScript server.
    fn forward_request(&mut self, primary: &Client, params: &Value) -> Result<()> {
        let Some(id) = params[0].as_u64() else {
            return Ok(());
        };
        let command = params[1].as_str().unwrap_or_default();
        if self.requests.len() >= MAX_BRIDGE_REQUESTS || !allowed_command(command) {
            return self.send_result(id, &Value::Null);
        }
        self.next_id += 1;
        let request_id = format!("vue-{}", self.next_id);
        primary.send(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "workspace/executeCommand",
            "params": {
                "command": "typescript.tsserverRequest",
                "arguments": [command, params[2], {"expectsResult": true}]
            }
        }))?;
        self.requests.insert(
            request_id,
            Forwarded {
                id,
                deadline: Instant::now() + INITIALIZE_WAIT,
            },
        );
        Ok(())
    }

    /// Volar expects a response even for unknown commands and failed TypeScript requests.
    fn send_result(&self, id: u64, result: &Value) -> Result<()> {
        // Volar passes a tuple as the notification's sole positional argument.
        self.client
            .notify("tsserver/response", json!([[id, result]]))
    }
}

/// Commands used by Volar 3 for project metadata, Vue completions and classification.
fn allowed_command(command: &str) -> bool {
    matches!(
        command,
        "_vue:projectInfo"
            | "_vue:collectExtractProps"
            | "_vue:getComponentDirectives"
            | "_vue:getComponentMeta"
            | "_vue:getComponentNames"
            | "_vue:getComponentProps"
            | "_vue:getComponentSlots"
            | "_vue:getElementAttrs"
            | "_vue:getElementNames"
            | "_vue:getImportPathForFile"
            | "_vue:getAutoImportSuggestions"
            | "_vue:resolveAutoImportCompletionEntry"
            | "_vue:isRefAtPosition"
            | "_vue:resolveModuleName"
            | "_vue:documentHighlights-full"
            | "_vue:encodedSemanticClassifications-full"
            | "_vue:quickinfo"
    )
}
