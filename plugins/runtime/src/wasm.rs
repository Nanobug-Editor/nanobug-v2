//! Core WASM ABI and independent interactive and business execution lanes.

use crate::Output;
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{
    DocumentSnapshot, ErrorKind, Input, Instance, MAX_MESSAGE, PluginEvent, Reply, Request,
    WorkerTiming, document_wire::MAX_DOCUMENT_BYTES, receive,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::BufRead,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    time::Instant,
};
use wasmtime::{
    Config, Engine, ExternType, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

/// Per-instance guest memory limit. Worker and host-owned resources have separate budgets.
const MEMORY: usize = 64 * 1024 * 1024;
/// Work ceilings apply to start functions, allocator calls and the guest event handler together.
const UI_FUEL: u64 = 6_000_000;
const BUSINESS_FUEL: u64 = 60_000_000;
const MAX_DOCUMENT_BASES: usize = 8;

/// Use the editor's private cache configuration. Missing or invalid cache config permits recompilation.
pub(super) fn engine(cache: Option<&Path>) -> Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true);
    if let Some(path) = cache {
        config.cache(wasmtime::Cache::from_file(Some(path)).ok());
    }
    Ok(Engine::new(&config)?)
}

/// Validate actual exports and imports before instantiation. This ABI passes capabilities as messages.
fn compile(engine: &Engine, path: &Path) -> Result<Module> {
    let module = Module::from_file(engine, path)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("invalid WASM: {}", path.display()))?;
    ensure!(module.imports().next().is_none(), "unavailable WASM import");
    let Some(ExternType::Memory(memory)) = module.get_export("memory") else {
        anyhow::bail!("WASM must export memory");
    };
    ensure!(
        !memory.is_64() && !memory.is_shared() && memory.minimum() <= (MEMORY / 65536) as u64,
        "unsupported WASM memory"
    );
    for (name, params, results) in [
        ("nb_alloc", &["i32"][..], &["i32"][..]),
        ("nb_run", &["i32", "i32"][..], &["i64"][..]),
        ("nb_free", &["i32", "i32"][..], &[][..]),
    ] {
        let Some(ExternType::Func(signature)) = module.get_export(name) else {
            anyhow::bail!("WASM must export {name}");
        };
        ensure!(
            signature
                .params()
                .map(|ty| ty.to_string())
                .eq(params.iter().copied())
                && signature
                    .results()
                    .map(|ty| ty.to_string())
                    .eq(results.iter().copied()),
            "wrong WASM ABI for {name}"
        );
    }
    Ok(module)
}

/// All live guest state for one project module in one execution lane.
struct Guest {
    store: Store<StoreLimits>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    run: TypedFunc<(i32, i32), i64>,
    free: TypedFunc<(i32, i32), ()>,
}

impl Guest {
    /// Instantiate verified code with private bounded memory and no native capabilities.
    fn new(engine: &Engine, module: &Module, fuel: u64) -> Result<Self> {
        let limits = StoreLimitsBuilder::new()
            .instances(1)
            .memories(1)
            .tables(4)
            .memory_size(MEMORY)
            .table_elements(64 * 1024)
            .build();
        let mut store = Store::new(engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(fuel)?;
        let instance = wasmtime::Instance::new(&mut store, module, &[])?;
        Ok(Self {
            memory: instance
                .get_memory(&mut store, "memory")
                .context("guest memory")?,
            alloc: instance.get_typed_func(&mut store, "nb_alloc")?,
            run: instance.get_typed_func(&mut store, "nb_run")?,
            free: instance.get_typed_func(&mut store, "nb_free")?,
            store,
        })
    }

    /// Copy a complete request and response through the guest's checked memory.
    fn call(&mut self, input: &Input, fuel: u64) -> Result<nanobug_plugin_sdk::Output> {
        let bytes = serde_json::to_vec(input)?;
        ensure!(
            bytes.len() <= MAX_MESSAGE,
            "WASM input exceeds message limit"
        );
        self.store.set_fuel(fuel)?;
        let len = bytes.len() as i32;
        let ptr = self.alloc.call(&mut self.store, len)?;
        let start = usize::try_from(ptr).context("invalid WASM allocation")?;
        self.memory.write(&mut self.store, start, &bytes)?;
        let value = self.run.call(&mut self.store, (ptr, len))? as u64;
        let address = (value >> 32) as usize;
        let count = (value as u32) as usize;
        ensure!(
            count <= MAX_MESSAGE / 2 && address <= i32::MAX as usize,
            "invalid WASM response"
        );
        let end = address + count;
        ensure!(
            end <= start || address >= start + bytes.len(),
            "WASM response overlaps its input"
        );
        let mut output = vec![0; count];
        self.memory.read(&self.store, address, &mut output)?;
        self.free.call(&mut self.store, (ptr, len))?;
        self.free
            .call(&mut self.store, (address as i32, count as i32))?;
        serde_json::from_slice(&output).context("invalid WASM response JSON")
    }
}

/// A close request completes after both lanes have dropped all of its project stores.
enum Job {
    Request(Request, Instant),
    TrimCache,
    Close {
        id: u64,
        project: u64,
        lanes: Arc<AtomicU8>,
    },
}

/// Recent bases are shared within a lane and dropped with their project or under memory pressure.
fn remember_document(
    documents: &mut VecDeque<(u64, DocumentSnapshot)>,
    project: u64,
    document: DocumentSnapshot,
) {
    if document.text.len() > MAX_DOCUMENT_BYTES {
        return;
    }
    documents.retain(|(owner, old)| {
        (*owner, old.opening, old.revision) != (project, document.opening, document.revision)
    });
    if documents.len() == MAX_DOCUMENT_BASES {
        documents.pop_front();
    }
    documents.push_back((project, document));
}

/// Serve one lane in request order. Compiled modules can be reused within this process.
fn lane(engine: Engine, requests: mpsc::Receiver<Job>, output: Output, fuel: u64) -> Result<()> {
    let mut modules = BTreeMap::<PathBuf, Module>::new();
    let mut guests = BTreeMap::<Instance, Guest>::new();
    let mut documents = VecDeque::<(u64, DocumentSnapshot)>::new();
    while let Ok(job) = requests.recv() {
        let mut timing = WorkerTiming {
            queue_us: match &job {
                Job::Request(_, queued) => elapsed_us(*queued),
                _ => 0,
            },
            ..WorkerTiming::default()
        };
        let (id, result) = match job {
            Job::TrimCache => {
                modules.clear();
                documents.clear();
                continue;
            }
            Job::Request(Request::Inspect { id, modules }, _) => {
                let result = modules
                    .iter()
                    .try_for_each(|path| compile(&engine, path).map(drop))
                    .map(|()| Reply::Inspected { id });
                (id, result)
            }
            Job::Request(
                Request::Open {
                    id,
                    instance,
                    wasm,
                    input,
                    interactive,
                },
                _,
            ) => {
                let result = (|| -> Result<_> {
                    ensure!(!guests.contains_key(&instance), "module already opened");
                    if !modules.contains_key(&wasm) {
                        let start = Instant::now();
                        modules.insert(wasm.clone(), compile(&engine, &wasm)?);
                        timing.compile_us = elapsed_us(start);
                    }
                    let start = Instant::now();
                    let mut guest = Guest::new(&engine, &modules[&wasm], fuel)?;
                    timing.initialize_us = elapsed_us(start);
                    let start = Instant::now();
                    let result = guest.call(&input, fuel)?;
                    timing.compute_us = elapsed_us(start);
                    if interactive && let Some(document) = input.document {
                        remember_document(&mut documents, instance.project, document);
                    }
                    guests.insert(instance, guest);
                    Ok(Reply::Result {
                        id,
                        output: Box::new(result),
                        timing: Some(timing),
                    })
                })();
                (id, result)
            }
            Job::Request(
                Request::Call {
                    id,
                    instance,
                    input,
                    ..
                },
                _,
            ) => {
                let start = Instant::now();
                let result = guests
                    .get_mut(&instance)
                    .context("module is closed")
                    .and_then(|guest| guest.call(&input, fuel))
                    .map(|result| {
                        timing.compute_us = elapsed_us(start);
                        Reply::Result {
                            id,
                            output: Box::new(result),
                            timing: Some(timing),
                        }
                    });
                (id, result)
            }
            Job::Request(
                Request::RefreshDocument {
                    id,
                    instance,
                    mut input,
                    delta,
                    ..
                },
                _,
            ) => {
                let result = (|| -> Result<_> {
                    ensure!(
                        matches!(input.event, PluginEvent::DocumentChanged),
                        "invalid document refresh"
                    );
                    let guest = guests.get_mut(&instance).context("module is closed")?;
                    if let Some(delta) = delta {
                        ensure!(input.document.is_none(), "duplicate document refresh input");
                        let base = documents.iter().find(|(project, base)| {
                            (*project, base.opening, base.revision)
                                == (instance.project, delta.opening, delta.base_revision)
                        });
                        let Some(document) = base.and_then(|(_, base)| delta.apply(base)) else {
                            return Ok(Reply::DocumentResync { id });
                        };
                        input.document = Some(document);
                    }
                    let start = Instant::now();
                    let result = guest.call(&input, fuel)?;
                    timing.compute_us = elapsed_us(start);
                    if let Some(document) = input.document {
                        remember_document(&mut documents, instance.project, document);
                    }
                    Ok(Reply::Result {
                        id,
                        output: Box::new(result),
                        timing: Some(timing),
                    })
                })();
                (id, result)
            }
            Job::Close { id, project, lanes } => {
                guests.retain(|instance, _| instance.project != project);
                documents.retain(|(owner, _)| *owner != project);
                if lanes.fetch_sub(1, Ordering::AcqRel) == 1 {
                    output.send(Reply::Closed { id, project })?;
                }
                continue;
            }
            _ => anyhow::bail!("wrong request for WASM worker"),
        };
        match result {
            Ok(reply) => output.send(reply)?,
            Err(error) => output.error(id, ErrorKind::Failed, format_args!("{error:#}"))?,
        }
    }
    Ok(())
}

/// Route UI and business traffic to separate bounded execution threads.
pub(super) fn serve(mut input: impl BufRead, output: Output, engine: Engine) -> Result<()> {
    let (ui, ui_rx) = mpsc::sync_channel(16);
    let (business, business_rx) = mpsc::sync_channel(16);
    for (receiver, fuel) in [(ui_rx, UI_FUEL), (business_rx, BUSINESS_FUEL)] {
        let engine = engine.clone();
        let output = output.clone();
        std::thread::spawn(move || {
            let _ = lane(engine, receiver, output, fuel);
        });
    }
    while let Some(request) = receive(&mut input)? {
        let (id, target) = match request {
            Request::Open {
                id, interactive, ..
            }
            | Request::Call {
                id, interactive, ..
            }
            | Request::RefreshDocument {
                id, interactive, ..
            } => (id, if interactive { &ui } else { &business }),
            Request::Inspect { id, .. } => (id, &business),
            Request::TrimCache => {
                for queue in [&ui, &business] {
                    let _ = queue.try_send(Job::TrimCache);
                }
                continue;
            }
            Request::Close { id, project } => {
                let lanes = Arc::new(AtomicU8::new(2));
                for queue in [&ui, &business] {
                    queue.send(Job::Close {
                        id,
                        project,
                        lanes: Arc::clone(&lanes),
                    })?;
                }
                continue;
            }
            _ => anyhow::bail!("wrong request for WASM worker"),
        };
        if target
            .try_send(Job::Request(request, Instant::now()))
            .is_err()
        {
            output.error(id, ErrorKind::Busy, "Plugin worker is busy")?;
        }
    }
    Ok(())
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
