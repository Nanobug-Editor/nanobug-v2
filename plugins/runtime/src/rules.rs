//! Independent QuickJS rules with cumulative execution and overall deadlines.

use crate::Output;
use nanobug_plugin_sdk::{ErrorKind, Query, Reply, Request, receive};
use rquickjs::{CaughtError, Context, Ctx, Function, Object, Promise, Runtime};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::BTreeMap,
    io::BufRead,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const LIMIT: usize = 4;
const MAX_QUERIES: usize = 32;
const COMPUTE: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(5);
type RuleError = (ErrorKind, String);
type Data = (u32, Result<Value, String>);

/// Each evaluation counts time spent in QuickJS separately from host waits.
struct Budget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    active: Cell<Option<Instant>>,
    used: Cell<Duration>,
}

impl Budget {
    fn check(&self) -> Result<(), RuleError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err((ErrorKind::Cancelled, "Rule evaluation cancelled".into()));
        }
        if Instant::now() >= self.deadline {
            return Err((ErrorKind::Deadline, "Rule exceeded five seconds".into()));
        }
        let elapsed = self
            .active
            .get()
            .map_or(Duration::ZERO, |start| start.elapsed());
        if self.used.get() + elapsed >= COMPUTE {
            return Err((
                ErrorKind::Computation,
                "Rule exhausted its JS execution budget".into(),
            ));
        }
        Ok(())
    }

    /// Run one JS step and report interrupt causes distinctly.
    fn step<T>(
        &self,
        context: &Ctx<'_>,
        operation: impl FnOnce() -> rquickjs::Result<T>,
    ) -> Result<T, RuleError> {
        self.check()?;
        let start = Instant::now();
        self.active.set(Some(start));
        let result = operation().map_err(|error| {
            let message = match CaughtError::from_error(context, error) {
                CaughtError::Exception(error) => error
                    .message()
                    .unwrap_or_else(|| "JavaScript exception".into()),
                CaughtError::Error(error) => error.to_string(),
                CaughtError::Value(_) => "JavaScript threw a value".into(),
            };
            (ErrorKind::Invalid, message.chars().take(512).collect())
        });
        self.active.set(None);
        self.used.set(self.used.get() + start.elapsed());
        self.check()?;
        result
    }
}

fn js_error(error: rquickjs::Error) -> RuleError {
    (ErrorKind::Invalid, error.to_string())
}

/// Run a rule body with the declared async host API, or only compile its syntax.
fn evaluate(
    id: u64,
    source: &str,
    context: Option<Value>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    data: mpsc::Receiver<Data>,
    output: &Output,
) -> Result<Reply, RuleError> {
    let budget = Rc::new(Budget {
        deadline,
        cancelled,
        active: Cell::new(None),
        used: Cell::new(Duration::ZERO),
    });
    let runtime = Runtime::new().map_err(js_error)?;
    runtime.set_memory_limit(8 * 1024 * 1024);
    let clock = Rc::clone(&budget);
    runtime.set_interrupt_handler(Some(Box::new(move || clock.check().is_err())));
    let cx = Context::full(&runtime).map_err(js_error)?;
    cx.with(|ctx| {
        let constructor: Function =
            budget.step(&ctx, || ctx.eval("(async function() {}).constructor"))?;
        let rule: Function = budget.step(&ctx, || constructor.call(("host", source)))?;
        let Some(context) = context else {
            return Ok(Reply::RuleChecked { id });
        };
        let bootstrap: Function = budget.step(&ctx, || ctx.eval(include_str!("rule_api.js")))?;
        let context: rquickjs::Value = budget.step(&ctx, || ctx.json_parse(context.to_string()))?;
        let bridge: Object = budget.step(&ctx, || bootstrap.call((context,)))?;
        let host: Object = bridge.get("host").map_err(js_error)?;
        let take: Function = bridge.get("take").map_err(js_error)?;
        let reply: Function = bridge.get("reply").map_err(js_error)?;
        let promise: Promise = budget.step(&ctx, || rule.call((host,)))?;
        let mut waiting = std::collections::BTreeSet::new();
        let mut issued = 0;
        loop {
            let result = budget.step(&ctx, || match promise.finish::<bool>() {
                Err(rquickjs::Error::WouldBlock) => Ok(None),
                other => other.map(Some),
            })?;
            if let Some(matches) = result {
                return Ok(Reply::Rule { id, matches });
            }
            let requests: String = budget.step(&ctx, || take.call(()))?;
            let requests: Vec<Value> =
                serde_json::from_str(&requests).map_err(|e| (ErrorKind::Invalid, e.to_string()))?;
            issued += requests.len();
            if issued > MAX_QUERIES {
                return Err((ErrorKind::Invalid, "Too many host queries".into()));
            }
            for request in requests {
                let query = request["id"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| (ErrorKind::Invalid, "Invalid host query ID".into()))?;
                let kind: Query = serde_json::from_value(request["kind"].clone())
                    .map_err(|error| (ErrorKind::Invalid, error.to_string()))?;
                if !waiting.insert(query) {
                    return Err((ErrorKind::Invalid, "Duplicate host query ID".into()));
                }
                output
                    .send(Reply::Query {
                        id,
                        query,
                        kind,
                        args: request["args"].clone(),
                    })
                    .map_err(|error| (ErrorKind::Failed, error.to_string()))?;
            }
            if waiting.is_empty() {
                return Err((
                    ErrorKind::Invalid,
                    "Rule awaits a promise without a host query".into(),
                ));
            }
            let received =
                data.recv_timeout(budget.deadline.saturating_duration_since(Instant::now()));
            budget.check()?;
            let (query, result) = received.map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    (ErrorKind::Deadline, "Rule exceeded five seconds".into())
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    (ErrorKind::Failed, "Host data connection closed".into())
                }
            })?;
            if waiting.remove(&query) {
                let value = serde_json::to_string(&result)
                    .map_err(|e| (ErrorKind::Invalid, e.to_string()))?;
                budget.step(&ctx, || reply.call::<_, ()>((query, value)))?;
            }
        }
    })
}

/// A cancelled query releases its waiting thread as soon as the host closes the sender.
struct Running {
    sender: Option<mpsc::SyncSender<Data>>,
    done: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

/// Accept only bounded concurrent evaluations, routing late query results to live owners.
pub(super) fn serve(mut input: impl BufRead, output: Output) -> anyhow::Result<()> {
    let mut running = BTreeMap::<u64, Running>::new();
    while let Some(request) = receive(&mut input)? {
        running.retain(|_, job| !job.done.load(Ordering::Acquire));
        let (id, source, context, duration) = match request {
            Request::Evaluate {
                id,
                source,
                context,
                remaining_ms,
            } => (
                id,
                source,
                Some(context),
                DEADLINE.min(Duration::from_millis(remaining_ms)),
            ),
            Request::CheckRule { id, source } => (id, source, None, DEADLINE),
            Request::Data { id, query, result } => {
                if let Some(sender) = running.get(&id).and_then(|job| job.sender.as_ref()) {
                    let _ = sender.try_send((query, result));
                }
                continue;
            }
            Request::CancelRule { id } => {
                if let Some(job) = running.get_mut(&id) {
                    job.cancelled.store(true, Ordering::Release);
                    job.sender.take();
                }
                continue;
            }
            _ => anyhow::bail!("wrong request for rule worker"),
        };
        if running.len() >= LIMIT || running.contains_key(&id) {
            output.error(id, ErrorKind::Busy, "Rule worker is busy")?;
            continue;
        }
        let (sender, data) = mpsc::sync_channel(MAX_QUERIES);
        let done = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let worker_cancelled = Arc::clone(&cancelled);
        let deadline = Instant::now() + duration;
        let output = output.clone();
        std::thread::spawn(move || {
            let result = evaluate(
                id,
                &source,
                context,
                deadline,
                worker_cancelled,
                data,
                &output,
            );
            worker_done.store(true, Ordering::Release);
            let _ = match result {
                Ok(reply) => output.send(reply),
                Err((kind, reason)) => output.error(id, kind, reason),
            };
        });
        running.insert(
            id,
            Running {
                sender: Some(sender),
                done,
                cancelled,
            },
        );
    }
    for job in running.values() {
        job.cancelled.store(true, Ordering::Release);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_adds_js_steps_without_counting_idle_time() {
        let budget = Budget {
            deadline: Instant::now() + DEADLINE,
            cancelled: Arc::new(AtomicBool::new(false)),
            active: Cell::new(None),
            used: Cell::new(Duration::from_millis(19)),
        };
        assert!(budget.check().is_ok());
        budget
            .active
            .set(Some(Instant::now() - Duration::from_millis(2)));
        assert_eq!(budget.check().unwrap_err().0, ErrorKind::Computation);
        budget.active.set(None);
        assert!(budget.check().is_ok());
        budget.used.set(COMPUTE);
        assert_eq!(budget.check().unwrap_err().0, ErrorKind::Computation);
    }
}
