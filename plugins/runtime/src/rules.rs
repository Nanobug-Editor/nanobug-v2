//! Independent QuickJS rules with cumulative execution and overall deadlines.

use crate::Output;
use nanobug_plugin_sdk::{ErrorKind, MAX_MESSAGE, Query, Reply, Request, receive};
use rquickjs::{
    CaughtError, Context, Ctx, Function, Module, Object, Runtime, module::Declared,
    promise::MaybePromise,
};
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
/// Bound the number of array elements decoded from one rule result.
const MAX_EXCLUDED_DIRS: usize = 128;
/// Keep the encoded rule reply below the wire limit even after JSON escaping.
const MAX_EXCLUSION_BYTES: usize = MAX_MESSAGE / 8;
const COMPUTE: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(5);
type RuleError = (ErrorKind, String);
type Data = (u32, Result<Value, String>);

/// A compiled rule body or a shared ES module with named activation functions.
enum CompiledRule<'js> {
    /// The legacy asynchronous function body.
    Body(Function<'js>),
    /// The ES module, compiled without executing its top-level code.
    Shared(Module<'js, Declared>),
}

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

/// Decode the activation flag, startup request and scan exclusions from the rule's `value`.
fn rule_result(value: rquickjs::Value<'_>) -> rquickjs::Result<(bool, bool, Vec<String>)> {
    if let Some(active) = value.as_bool() {
        return Ok((active, false, Vec::new()));
    }
    let object = value.as_object().ok_or_else(|| {
        rquickjs::Error::new_from_js_message(
            "JavaScript rule return value",
            "boolean or rule object",
            "Expected a boolean or an object with an active boolean",
        )
    })?;
    let active = object.get::<_, bool>("active")?;
    let startup = object.get::<_, rquickjs::Value>("startLanguageServer")?;
    let requested = if startup.is_undefined() {
        false
    } else {
        startup
            .as_bool()
            .ok_or_else(|| rquickjs::Error::new_from_js("startLanguageServer", "boolean"))?
    };
    let exclude_dirs = rule_exclude_dirs(object.get("excludeDirs")?)?;
    Ok((
        active,
        active && requested,
        if active { exclude_dirs } else { Vec::new() },
    ))
}

/// Decode the optional exclusion array `value` within the rule reply's transport budget.
fn rule_exclude_dirs(value: rquickjs::Value<'_>) -> rquickjs::Result<Vec<String>> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    let array = value
        .as_array()
        .ok_or_else(|| rquickjs::Error::new_from_js("excludeDirs", "array of strings"))?;
    if array.len() > MAX_EXCLUDED_DIRS {
        return Err(rquickjs::Error::new_from_js_message(
            "excludeDirs",
            "array of strings",
            format!("Expected at most {MAX_EXCLUDED_DIRS} entries"),
        ));
    }
    let mut remaining = MAX_EXCLUSION_BYTES;
    array
        .iter::<String>()
        .map(|entry| {
            let path = entry?;
            remaining = remaining.checked_sub(path.len()).ok_or_else(|| {
                rquickjs::Error::new_from_js_message(
                    "excludeDirs",
                    "array of strings",
                    "Exclusion paths exceed the rule reply size limit",
                )
            })?;
            Ok(path)
        })
        .collect()
}

/// Select the own callable property of a shared module's default export by `id`.
fn activation_function<'js>(
    module: Module<'js, Declared>,
    id: &str,
) -> rquickjs::Result<Function<'js>> {
    let (module, ready) = module.eval()?;
    ready.finish::<()>()?;
    let rules: Object = module.get("default")?;
    if rules
        .keys::<String>()
        .any(|key| key.is_ok_and(|key| key == id))
    {
        let value: rquickjs::Value = rules.get(id)?;
        if let Some(function) = value.into_function() {
            return Ok(function);
        }
    }
    Err(rquickjs::Error::new_from_js_message(
        "shared rule default export",
        "function",
        format!("No activation function for module '{id}'"),
    ))
}

/// Compile the rule in its existing scope, or recognize an ES module if the body does not parse.
fn compile_rule<'js>(
    budget: &Budget,
    ctx: &Ctx<'js>,
    source: &str,
) -> Result<CompiledRule<'js>, RuleError> {
    let constructor: Function =
        budget.step(ctx, || ctx.eval("(async function() {}).constructor"))?;
    match budget.step(ctx, || constructor.call(("host", source))) {
        Ok(rule) => Ok(CompiledRule::Body(rule)),
        Err((ErrorKind::Invalid, _)) => budget
            .step(ctx, || Module::declare(ctx.clone(), "nanobug-rule", source))
            .map(CompiledRule::Shared),
        Err(error) => Err(error),
    }
}

/// Run one activation rule with its host context, or validate the script syntax.
fn evaluate(
    id: u64,
    source: &str,
    module_id: Option<&str>,
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
        let compiled = compile_rule(&budget, &ctx, source)?;
        let Some(context) = context else {
            return Ok(Reply::RuleChecked { id });
        };
        let rule = match compiled {
            CompiledRule::Body(rule) => rule,
            CompiledRule::Shared(module) => {
                let id = module_id
                    .ok_or_else(|| (ErrorKind::Invalid, "Shared rule needs a module ID".into()))?;
                budget.step(&ctx, || activation_function(module, id))?
            }
        };
        let bootstrap: Function = budget.step(&ctx, || ctx.eval(include_str!("rule_api.js")))?;
        let context: rquickjs::Value = budget.step(&ctx, || ctx.json_parse(context.to_string()))?;
        let bridge: Object = budget.step(&ctx, || bootstrap.call((context,)))?;
        let host: Object = bridge.get("host").map_err(js_error)?;
        let take: Function = bridge.get("take").map_err(js_error)?;
        let reply: Function = bridge.get("reply").map_err(js_error)?;
        let promise: MaybePromise = budget.step(&ctx, || rule.call((host,)))?;
        let mut waiting = std::collections::BTreeSet::new();
        let mut issued = 0;
        loop {
            let result = budget.step(&ctx, || match promise.finish::<rquickjs::Value>() {
                Err(rquickjs::Error::WouldBlock) => Ok(None),
                Ok(value) => rule_result(value).map(Some),
                Err(error) => Err(error),
            })?;
            if let Some((matches, start_language_server, exclude_dirs)) = result {
                return Ok(Reply::Rule {
                    id,
                    matches,
                    start_language_server,
                    exclude_dirs,
                });
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
        let (id, source, module, context, duration) = match request {
            Request::Evaluate {
                id,
                source,
                module,
                context,
                remaining_ms,
            } => (
                id,
                source,
                module,
                Some(context),
                DEADLINE.min(Duration::from_millis(remaining_ms)),
            ),
            Request::CheckRule { id, source } => (id, source, None, None, DEADLINE),
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
                module.as_deref(),
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
