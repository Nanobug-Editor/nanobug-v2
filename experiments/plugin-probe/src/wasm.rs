//! Compile the trusted sample once and retain an independently droppable store per project.

use crate::Output;
use anyhow::{Context as _, Result, ensure};
use nanobug_probe_wire::{Draft, Event, Failure, MAX_DRAFT, Owner, Request, receive};
use std::{
    collections::HashMap,
    io::BufRead,
    sync::{Arc, Mutex, mpsc},
    time::Instant,
};
use wasmtime::{
    Config, Engine, Instance, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap,
    TypedFunc,
};

/// The only guest artifact accepted by the experiment.
const FIXTURE: &str = include_str!("../fixtures/plugin.wat");
/// Fuel budget for instantiation and one complete drawing request.
const DRAW_FUEL: u64 = 100_000;
/// Fuel budget for one background job, independent of the drawing lane.
const WORK_FUEL: u64 = 500_000_000;
/// Guest memory offset of the host's draft staging area.
const INPUT: usize = 512;
/// Guest memory offset of the requested text after executing `draw`.
const OUTPUT: usize = 2048;

/// Live instance, scoped to one opening of one project.
struct Surface {
    /// Host owner to which all frames and resource releases belong.
    owner: Owner,
    /// Last revision rendered, preventing out-of-order replacement.
    revision: u64,
    /// Owns the guest memory and functions. Dropping it releases the instance.
    store: Store<StoreLimits>,
    /// Export used to transfer bounded input and draw text.
    memory: Memory,
    /// Guest function which writes the complete text draw request.
    draw: TypedFunc<i32, i32>,
    /// Guest-provided heading, unchanged for this instance.
    title: String,
    /// Guest-provided background RGBA value.
    color: u32,
    /// Business jobs that still own this scope, including queued work.
    jobs: Arc<Mutex<Jobs>>,
}

/// Counts the business lane's references without holding a lock while WASM executes.
#[derive(Default)]
struct Jobs {
    /// Number of queued or running jobs that must release the scope before Close is acknowledged.
    pending: usize,
    /// Instance count after this scope's UI store was dropped, or none while it is still open.
    closed: Option<usize>,
}

/// One queued business invocation tied to a live scope.
struct Job {
    /// Target scope and runtime generation.
    owner: Owner,
    /// Use the deterministic fuel-exhaustion fixture.
    spin: bool,
    /// Shared ownership record used to drain or skip work on close.
    jobs: Arc<Mutex<Jobs>>,
}

impl Surface {
    /// Instantiate the compiled sample for `owner`, with a private store and memory.
    fn new(engine: &Engine, module: &Module, owner: Owner) -> Result<Self> {
        let (mut store, instance) = instantiate(engine, module, DRAW_FUEL)?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("guest memory")?;
        let draw = instance.get_typed_func(&mut store, "draw")?;
        let len = instance
            .get_typed_func::<(), i32>(&mut store, "title_len")?
            .call(&mut store, ())?;
        ensure!((1..=80).contains(&len), "invalid guest title length");
        let title = copy_text(&memory, &store, 0, len as usize)?;
        let color = instance
            .get_typed_func::<(), i32>(&mut store, "background")?
            .call(&mut store, ())? as u32;
        Ok(Self {
            owner,
            revision: 0,
            store,
            memory,
            draw,
            title,
            color,
            jobs: Arc::default(),
        })
    }

    /// Run the guest with the latest `draft` and emit its complete frame for host layout.
    fn render(&mut self, draft: Draft, instances: usize) -> Result<Event> {
        ensure!(draft.revision >= self.revision, "stale draft revision");
        ensure!(draft.text.len() <= MAX_DRAFT, "draft exceeds sample limit");
        self.store.set_fuel(DRAW_FUEL)?;
        self.memory
            .write(&mut self.store, INPUT, draft.text.as_bytes())?;
        let len = self.draw.call(&mut self.store, draft.text.len() as i32)?;
        ensure!(
            (0..=MAX_DRAFT as i32).contains(&len),
            "invalid guest text length"
        );
        let text = copy_text(&self.memory, &self.store, OUTPUT, len as usize)?;
        self.revision = draft.revision;
        Ok(Event::Frame {
            owner: self.owner,
            draft: Draft {
                revision: draft.revision,
                text,
            },
            title: self.title.clone(),
            color: self.color,
            instances,
        })
    }

    /// Drop the UI store, deferring acknowledgment while any job still owns the scope.
    fn close(self, instances: usize) -> Option<Event> {
        let owner = self.owner;
        let jobs = Arc::clone(&self.jobs);
        drop(self);
        let mut jobs = jobs.lock().unwrap();
        jobs.closed = Some(instances);
        (jobs.pending == 0).then_some(Event::Closed { owner, instances })
    }

    /// Queue a job before releasing the lock that lets it begin.
    fn queue_work(&self, spin: bool, queue: &mpsc::SyncSender<Job>) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let job = Job {
            owner: self.owner,
            spin,
            jobs: Arc::clone(&self.jobs),
        };
        if queue.try_send(job).is_err() {
            return false;
        }
        jobs.pending += 1;
        true
    }
}

/// Copy valid one-line text at `offset` and `len` from the guest's bounded memory.
fn copy_text(
    memory: &Memory,
    store: &Store<StoreLimits>,
    offset: usize,
    len: usize,
) -> Result<String> {
    let mut bytes = vec![0; len];
    memory.read(store, offset, &mut bytes)?;
    let text = String::from_utf8(bytes)?;
    ensure!(
        !text.chars().any(char::is_control),
        "guest text must be a single line"
    );
    Ok(text)
}

/// Allocate a bounded store for `module` and give this invocation `fuel` instructions.
fn instantiate(
    engine: &Engine,
    module: &Module,
    fuel: u64,
) -> Result<(Store<StoreLimits>, Instance)> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(2 * 65536)
        .instances(1)
        .memories(1)
        .build();
    let mut store = Store::new(engine, limits);
    store.limiter(|limits| limits);
    store.set_fuel(fuel)?;
    let instance = Instance::new(&mut store, module, &[])?;
    Ok((store, instance))
}

/// Run the fixture's business job without borrowing any UI instance or store.
fn run_work(engine: &Engine, module: &Module, spin: bool) -> Result<u32, Failure> {
    let (mut store, instance) = instantiate(engine, module, WORK_FUEL)
        .map_err(|error| Failure::Error(error.to_string()))?;
    let work = instance
        .get_typed_func::<i32, u32>(&mut store, "work")
        .map_err(|error| Failure::Error(error.to_string()))?;
    work.call(&mut store, i32::from(spin)).map_err(|error| {
        if error.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
            Failure::WasmBudget
        } else {
            Failure::Error(error.to_string())
        }
    })
}

/// Release `job` after execution or skip it if its project has already closed.
fn perform(job: Job, engine: &Engine, module: &Module, output: &Output) -> std::io::Result<()> {
    let Job { owner, spin, jobs } = job;
    let cancelled = jobs.lock().unwrap().closed.is_some();
    let result = if cancelled {
        None
    } else {
        if spin {
            output.send(Event::WorkStarted { owner })?;
        }
        let start = Instant::now();
        let result = run_work(engine, module, spin);
        Some(Event::WorkDone {
            owner,
            spin,
            result,
            elapsed_us: start.elapsed().as_micros() as u64,
        })
    };
    // The store is gone by this point. A closed scope's queued work allocates no new store.
    let mut jobs = jobs.lock().unwrap();
    jobs.pending -= 1;
    let reply = match jobs.closed {
        Some(instances) if jobs.pending == 0 => Some(Event::Closed { owner, instances }),
        Some(_) => None,
        None => result,
    };
    drop(jobs);
    reply.map_or(Ok(()), |event| output.send(event))
}

/// Serve `input` through `output`, optionally injecting a retired frame after recovery.
pub(super) fn serve(
    mut input: impl BufRead,
    output: Output,
    inject_retired_frame: bool,
) -> Result<()> {
    let engine = Engine::new(Config::new().consume_fuel(true))?;
    let module = Module::new(&engine, FIXTURE)?;
    let (queue, jobs) = mpsc::sync_channel(4);
    let worker_engine = engine.clone();
    let worker_module = module.clone();
    let worker_output = output.clone();
    std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            if perform(job, &worker_engine, &worker_module, &worker_output).is_err() {
                break;
            }
        }
    });

    let mut surfaces: HashMap<u64, Surface> = HashMap::new();
    while let Some(request) = receive(&mut input)? {
        let instances = surfaces.len();
        let (owner, reply) = match request {
            Request::Open { owner, draft } if !surfaces.contains_key(&owner.scope) => {
                let reply = Surface::new(&engine, &module, owner).and_then(|mut surface| {
                    let frame = surface.render(draft, instances + 1)?;
                    if inject_retired_frame
                        && owner.generation > 1
                        && let Event::Frame {
                            draft,
                            color,
                            instances,
                            ..
                        } = &frame
                    {
                        output.send(Event::Frame {
                            owner: Owner {
                                generation: owner.generation - 1,
                                ..owner
                            },
                            draft: draft.clone(),
                            title: "Retired frame".into(),
                            color: *color,
                            instances: *instances,
                        })?;
                    }
                    surfaces.insert(owner.scope, surface);
                    Ok(frame)
                });
                (owner, reply)
            }
            Request::Open { owner, .. } => (owner, Err(anyhow::anyhow!("scope is already open"))),
            Request::Input { owner, draft } => (
                owner,
                surfaces
                    .get_mut(&owner.scope)
                    .filter(|surface| surface.owner == owner)
                    .context("scope is closed or stale")
                    .and_then(|surface| surface.render(draft, instances)),
            ),
            Request::Close { owner } => {
                let reply = if surfaces
                    .get(&owner.scope)
                    .is_some_and(|surface| surface.owner == owner)
                {
                    let surface = surfaces.remove(&owner.scope).expect("matching surface");
                    match surface.close(surfaces.len()) {
                        Some(released) => Ok(released),
                        None => continue,
                    }
                } else {
                    Err(anyhow::anyhow!("scope is closed or stale"))
                };
                (owner, reply)
            }
            Request::Work { owner, spin } => {
                if let Some(surface) = surfaces
                    .get(&owner.scope)
                    .filter(|surface| surface.owner == owner)
                {
                    if surface.queue_work(spin, &queue) {
                        continue;
                    }
                    (
                        owner,
                        Ok(Event::WorkDone {
                            owner,
                            spin,
                            result: Err(Failure::Busy),
                            elapsed_us: 0,
                        }),
                    )
                } else {
                    (owner, Err(anyhow::anyhow!("scope is closed or stale")))
                }
            }
            _ => anyhow::bail!("request belongs to the rules worker"),
        };
        output.send(reply.unwrap_or_else(|error| Event::ScopeError {
            owner,
            message: error.to_string(),
        }))?;
    }
    Ok(())
}
