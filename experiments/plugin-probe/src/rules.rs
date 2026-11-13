//! Run independent rule contexts with a bounded number of waiting or executing evaluations.

use crate::Output;
use nanobug_probe_wire::{Event, Failure, MAX_FLAG, RULE_LANES, Request, Rule, receive};
use rquickjs::{Context, Promise, Runtime};
use std::{
    cell::Cell,
    collections::HashMap,
    io::BufRead,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

/// Maximum elapsed execution time summed across JS slices, excluding host-data waits.
const JS_BUDGET: Duration = Duration::from_millis(20);
/// Maximum wall time for an evaluation, including the host-data wait.
const DEADLINE: Duration = Duration::from_secs(5);
/// The probe's allowed asynchronous file read and boolean check.
const ENABLED: &str = include_str!("../fixtures/enabled.js");

/// Accounting for one evaluation on its own worker thread.
struct Budget {
    /// Start time used for the overall deadline.
    started: Instant,
    /// Total elapsed time of finished JS slices.
    used: Cell<Duration>,
    /// Active JS slice, absent while awaiting data.
    active: Cell<Option<Instant>>,
}

impl Budget {
    /// Wrap one JavaScript step in execution accounting and normalize interrupt failures.
    fn js<T>(&self, execute: impl FnOnce() -> rquickjs::Result<T>) -> Result<T, Failure> {
        self.check()?;
        let started = Instant::now();
        self.active.set(Some(started));
        let result = execute();
        self.active.set(None);
        self.used.set(self.used.get() + started.elapsed());
        self.check()?;
        result.map_err(|error| Failure::Error(error.to_string()))
    }

    /// Check both clocks, counting an active JS slice if the engine is currently executing.
    fn check(&self) -> Result<(), Failure> {
        if self.started.elapsed() >= DEADLINE {
            return Err(Failure::RuleDeadline);
        }
        let active = self
            .active
            .get()
            .map_or(Duration::ZERO, |start| start.elapsed());
        if self.used.get() + active >= JS_BUDGET {
            return Err(Failure::JsBudget);
        }
        Ok(())
    }
}

/// One evaluation's reply channel and owned thread.
struct Evaluation {
    /// Carries at most one bounded file result from the host.
    reply: SyncSender<Result<String, String>>,
    /// Releases the slot after JS has dropped its context, before publishing the result.
    finished: Arc<AtomicBool>,
}

/// Evaluate `rule` for `id`, using `data` for the one permitted host result.
fn evaluate(
    id: u64,
    rule: Rule,
    data: Receiver<Result<String, String>>,
    output: &Output,
    budget: &Rc<Budget>,
) -> Result<bool, Failure> {
    let js_error = |error: rquickjs::Error| Failure::Error(error.to_string());
    let runtime = Runtime::new().map_err(js_error)?;
    runtime.set_memory_limit(4 * 1024 * 1024);
    let interrupt = Rc::clone(budget);
    runtime.set_interrupt_handler(Some(Box::new(move || interrupt.check().is_err())));
    let context = Context::full(&runtime).map_err(js_error)?;
    context.with(|ctx| {
        if matches!(rule, Rule::Busy) {
            return budget.js(|| ctx.eval::<bool, _>("while (true) {}"));
        }

        let (promise, resolve, _) = ctx.promise().map_err(js_error)?;
        // Keep the promise in JS so a Rust closure cannot retain the context that owns it.
        ctx.globals().set("probeFlag", promise).map_err(js_error)?;
        let result: Promise<'_> = budget.js(|| {
            ctx.eval::<(), _>(
                "var flagRequested = false; function readProbeFlag() { flagRequested = true; return probeFlag; }",
            )?;
            ctx.eval(ENABLED)
        })?;
        if !ctx.globals().get::<_, bool>("flagRequested").map_err(js_error)? {
            return budget.js(|| result.finish());
        }
        output
            .send(Event::ReadFlag { id })
            .map_err(|error| Failure::Error(error.to_string()))?;
        let value = data
            .recv_timeout(DEADLINE.saturating_sub(budget.started.elapsed()))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => Failure::RuleDeadline,
                RecvTimeoutError::Disconnected => Failure::Error("host closed the query".into()),
            })?
            .map_err(Failure::Error)?;
        if value.len() > MAX_FLAG {
            return Err(Failure::Error("probe flag response too large".into()));
        }
        budget.js(|| {
            resolve.call::<_, ()>((value,))?;
            result.finish()
        })
    })
}

/// Read evaluation and file-result requests from `input` and send results to `output`.
pub(super) fn serve(mut input: impl BufRead, output: Output) -> anyhow::Result<()> {
    let mut evaluations: HashMap<u64, Evaluation> = HashMap::new();
    while let Some(request) = receive(&mut input)? {
        evaluations.retain(|_, evaluation| !evaluation.finished.load(Ordering::Acquire));
        match request {
            Request::Evaluate { id, rule } => {
                if evaluations.contains_key(&id) || evaluations.len() >= RULE_LANES {
                    output.send(Event::RuleDone {
                        id,
                        result: Err(Failure::Busy),
                        js_us: 0,
                        total_us: 0,
                    })?;
                    continue;
                }
                let (reply, data) = mpsc::sync_channel(1);
                let output = output.clone();
                let finished = Arc::new(AtomicBool::new(false));
                let completed = Arc::clone(&finished);
                std::thread::spawn(move || {
                    let budget = Rc::new(Budget {
                        started: Instant::now(),
                        used: Cell::new(Duration::ZERO),
                        active: Cell::new(None),
                    });
                    let result = evaluate(id, rule, data, &output, &budget);
                    completed.store(true, Ordering::Release);
                    let _ = output.send(Event::RuleDone {
                        id,
                        result,
                        js_us: budget.used.get().as_micros() as u64,
                        total_us: budget.started.elapsed().as_micros() as u64,
                    });
                });
                evaluations.insert(id, Evaluation { reply, finished });
            }
            Request::FileResult { id, result } => {
                if let Some(evaluation) = evaluations.get(&id) {
                    let _ = evaluation.reply.try_send(result);
                }
            }
            _ => anyhow::bail!("request belongs to the WASM worker"),
        }
    }
    Ok(())
}
