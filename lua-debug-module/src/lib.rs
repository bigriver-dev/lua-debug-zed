//! package.cpath = package.cpath .. ";C:/path/to/?.dll"  -- folder holding luadebug.dll
//! local luadebug = require("luadebug")
//! luadebug.start({
//!     port = 8172,            -- default 8172 (host defaults to 127.0.0.1)
//!     wait = true,            -- block until the debugger has attached (default true)
//!     sourceRoot = "...",     -- folder relative chunk names (@main.lua) resolve against
//!     errorHandler = "...",   -- global(s) the host calls as its xpcall message handler
//!     onTerminate = ...,      -- how Zed's Stop ends the host: a function or a global's name
//! })
//!
//! sourceRoot, errorHandler and onTerminate can come from the debug.json entry instead,
//! which wins over start()'s.

#![allow(dead_code)]

// Compiled from `lua-dap-server`
#[path = "../../lua-dap-server/src/dap/mod.rs"]
mod dap;
#[path = "../../lua-dap-server/src/engine/mod.rs"]
mod engine;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Receiver, unbounded};
use mlua::{FromLua, Function, HookTriggers, Lua, MultiValue, Table, Value, VmState};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

use dap::session::{AttachedEngine, DapSession};
use engine::breakpoints::{BreakpointRegistry, FunctionBreakpointRegistry};
use engine::evaluator::Evaluator;
use engine::runner::{ExecutionCommand, LuaRunner, RunControl, RunnerEvent, StepMode};

const DEFAULT_PORT: u16 = 8172;

/*
 * shared with `luadebug.exception`
 */
struct Engine {
    control: Arc<RunControl>,
    exception_enabled: Arc<Mutex<bool>>,
    step_mode: Arc<Mutex<StepMode>>,
    stack_depth: Arc<Mutex<usize>>,
    cmd_rx: Receiver<ExecutionCommand>,
    event_tx: UnboundedSender<RunnerEvent>,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

#[derive(Clone)]
enum OnTerminate {
    Function(Function),
    Global(String),
}

/*
 * How the debugger plugs into the host, from start() and then each client's debug.json
 */
#[derive(Default)]
struct HostOptions {
    source_root: Option<String>,
    error_handlers: Vec<String>,
    on_terminate: Option<OnTerminate>,
}

impl HostOptions {
    fn from_table(opts: &Option<Table>) -> mlua::Result<Self> {
        let on_terminate = match opt::<Value>(opts, "onTerminate")? {
            None | Some(Value::Nil) => None,
            Some(Value::Function(f)) => Some(OnTerminate::Function(f)),
            Some(Value::String(s)) => Some(OnTerminate::Global(s.to_str()?.to_string())),
            Some(_) => {
                return Err(mlua::Error::RuntimeError(
                    "luadebug: onTerminate must be a function or a global function's name".into(),
                ));
            }
        };
        let error_handlers = match opt::<Value>(opts, "errorHandler")? {
            None | Some(Value::Nil) => Vec::new(),
            Some(Value::String(s)) => vec![s.to_str()?.to_string()],
            Some(Value::Table(t)) => t.sequence_values::<String>().collect::<mlua::Result<_>>()?,
            Some(_) => {
                return Err(mlua::Error::RuntimeError(
                    "luadebug: errorHandler must be a global function's name, or a list of them"
                        .into(),
                ));
            }
        };
        Ok(Self {
            source_root: opt(opts, "sourceRoot")?,
            error_handlers,
            on_terminate,
        })
    }

    fn from_json(args: &serde_json::Value) -> Self {
        let text = |key: &str| {
            args[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let error_handlers = match &args["errorHandler"] {
            serde_json::Value::Array(names) => names
                .iter()
                .filter_map(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect(),
            _ => text("errorHandler").into_iter().collect(),
        };
        Self {
            source_root: text("sourceRoot"),
            error_handlers,
            on_terminate: text("onTerminate").map(OnTerminate::Global),
        }
    }
}

/*
 * What's been applied to the host so far (Lua thread only)
 */
#[derive(Default)]
struct HostState {
    on_terminate: Option<OnTerminate>,
    // kept wrapped: the host may define them after start(), or replace them later
    error_handlers: Vec<(String, Option<Function>)>,
    // set while a wrapper runs, so a handler that calls the one it replaced stops once
    handling: Rc<Cell<bool>>,
    hook_calls: u32,
}

#[mlua::lua_module]
fn luadebug(lua: &Lua) -> mlua::Result<Table> {
    let exports = lua.create_table()?;
    exports.set("start", lua.create_function(start)?)?;
    exports.set(
        "exception",
        lua.create_function(|lua, (err, skip): (Value, Option<usize>)| {
            stop_on_exception(lua, &err, skip.unwrap_or(0));
            Ok(err)
        })?,
    )?;
    Ok(exports)
}

fn opt<T: FromLua>(opts: &Option<Table>, key: &str) -> mlua::Result<Option<T>> {
    match opts {
        Some(t) => t.get::<Option<T>>(key),
        None => Ok(None),
    }
}

/*
 * Hooks the host's Lua state and starts the DAP server on a background thread
 */
fn start(lua: &Lua, opts: Option<Table>) -> mlua::Result<()> {
    if ENGINE.get().is_some() {
        return Ok(());
    }
    let host: String = opt(&opts, "host")?.unwrap_or_else(|| "127.0.0.1".to_string());
    let launched_port: Option<u16> = std::env::var("LUADEBUG_PORT")
        .ok()
        .and_then(|p| p.trim().parse().ok());
    let port: u16 = match launched_port {
        Some(p) => p,
        None => opt(&opts, "port")?.unwrap_or(DEFAULT_PORT),
    };
    let wait: bool = opt(&opts, "wait")?.unwrap_or(true);
    let host_options = HostOptions::from_table(&opts)?;

    // launching debugger shows our output in Zed's console through pipes
    if launched_port.is_some() {
        let _ = lua
            .load("if io then for _, f in ipairs({ io.stdout, io.stderr }) do f:setvbuf('no') end end")
            .exec();
    }

    // Bind before hooking anything, so a busy port is a clear error at the call site
    let listener = std::net::TcpListener::bind((host.as_str(), port)).map_err(|e| {
        mlua::Error::RuntimeError(format!("luadebug: can't listen on {host}:{port}: {e}"))
    })?;
    listener.set_nonblocking(true).map_err(mlua::Error::external)?;

    let breakpoints = Arc::new(Mutex::new(BreakpointRegistry::new()));
    let function_breakpoints = Arc::new(Mutex::new(FunctionBreakpointRegistry::new()));
    let exception_enabled = Arc::new(Mutex::new(true));
    let control = Arc::new(RunControl::default());
    let step_mode = Arc::new(Mutex::new(StepMode::None));
    let stack_depth = Arc::new(Mutex::new(0usize));
    let (cmd_tx, cmd_rx) = unbounded();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = unbounded::<()>();
    let (attach_tx, attach_rx) = unbounded::<serde_json::Value>();

    let _ = ENGINE.set(Engine {
        control: Arc::clone(&control),
        exception_enabled: Arc::clone(&exception_enabled),
        step_mode: Arc::clone(&step_mode),
        stack_depth: Arc::clone(&stack_depth),
        cmd_rx: cmd_rx.clone(),
        event_tx: event_tx.clone(),
    });

    let state = Rc::new(RefCell::new(HostState::default()));
    apply_host_options(lua, &state, host_options).map_err(mlua::Error::RuntimeError)?;

    // the debug hook: the same engine callback lua-dap-server uses
    {
        let breakpoints = Arc::clone(&breakpoints);
        let function_breakpoints = Arc::clone(&function_breakpoints);
        let control = Arc::clone(&control);
        let no_entry_stop = Arc::new(Mutex::new(false));
        let terminate_handled = AtomicBool::new(false);
        let state = Rc::clone(&state);
        let attach_rx = attach_rx.clone();
        let event_tx = event_tx.clone();
        lua.set_hook(
            HookTriggers {
                every_line: true,
                on_calls: true,
                on_returns: true,
                ..Default::default()
            },
            move |lua, debug| {
                // a client that attached while the host runs
                if !attach_rx.is_empty() {
                    apply_client_options(lua, &state, &attach_rx, &event_tx);
                }
                {
                    let mut s = state.borrow_mut();
                    s.hook_calls = s.hook_calls.wrapping_add(1);
                    let recheck = !s.error_handlers.is_empty() && s.hook_calls % 64 == 0;
                    drop(s);
                    if recheck {
                        let _ = ensure_error_handlers(lua, &state);
                    }
                }
                if control.terminate_requested.load(Ordering::Relaxed) {
                    if terminate_handled.swap(true, Ordering::Relaxed) {
                        return Ok(VmState::Continue);
                    }
                    let on_terminate = state.borrow().on_terminate.clone();
                    let ended = match on_terminate {
                        Some(OnTerminate::Function(f)) => f.call::<()>(()).is_ok(),
                        Some(OnTerminate::Global(name)) => global_function(lua, &name)
                            .and_then(|f| f.call::<()>(()).map_err(|e| e.to_string()))
                            .is_ok(),
                        None => false,
                    };
                    if ended {
                        return Ok(VmState::Continue);
                    }
                }
                // A client that left mid-step mustn't leave a step pending
                if !control.client_attached.load(Ordering::Relaxed) {
                    let mut mode = step_mode.lock();
                    if *mode != StepMode::None {
                        *mode = StepMode::None;
                    }
                }
                LuaRunner::on_debug_hook(
                    lua,
                    &debug,
                    &breakpoints,
                    &function_breakpoints,
                    &step_mode,
                    &stack_depth,
                    &no_entry_stop,
                    Path::new(""),
                    &control,
                    &cmd_rx,
                    &event_tx,
                )?;
                Ok(VmState::Continue)
            },
        )?;
    }

    let engine = AttachedEngine {
        breakpoints,
        function_breakpoints,
        exception_breakpoints_enabled: exception_enabled,
        control,
        cmd_sender: cmd_tx,
        event_receiver: event_rx,
        on_configured: Some(ready_tx),
        on_attach: Some(attach_tx),
    };
    std::thread::Builder::new()
        .name("luadebug-dap".to_string())
        .spawn(move || serve(listener, engine))
        .map_err(mlua::Error::external)?;

    if wait {
        println!("luadebug: waiting for a debugger on {host}:{port}");
        let _ = ready_rx.recv();
        // before the host's code runs, so sourceRoot is in place for its first line
        apply_client_options(lua, &state, &attach_rx, &event_tx);
    } else {
        println!("luadebug: listening on {host}:{port}");
    }
    Ok(())
}

fn apply_host_options(
    lua: &Lua,
    state: &RefCell<HostState>,
    options: HostOptions,
) -> Result<(), String> {
    // The engine resolves relative chunk names (`@main.lua`) against the working directory
    if let Some(dir) = &options.source_root {
        std::env::set_current_dir(dir)
            .map_err(|e| format!("luadebug: bad sourceRoot '{dir}': {e}"))?;
    }
    if !options.error_handlers.is_empty() {
        for name in &options.error_handlers {
            global_slot(lua, name)?;
        }
        state.borrow_mut().error_handlers =
            options.error_handlers.into_iter().map(|n| (n, None)).collect();
        ensure_error_handlers(lua, state)?;
    }
    if let Some(on_terminate) = options.on_terminate {
        state.borrow_mut().on_terminate = Some(on_terminate);
    }
    Ok(())
}

// a problem in a client's options is reported in its console, not raised in the host
fn apply_client_options(
    lua: &Lua,
    state: &RefCell<HostState>,
    attach_rx: &Receiver<serde_json::Value>,
    event_tx: &UnboundedSender<RunnerEvent>,
) {
    for args in attach_rx.try_iter() {
        if let Err(message) = apply_host_options(lua, state, HostOptions::from_json(&args)) {
            let _ = event_tx.send(RunnerEvent::Output(format!("{message}\n")));
        }
    }
}

/*
 * One client at a time
 */
fn serve(listener: std::net::TcpListener, mut engine: AttachedEngine) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("luadebug: can't start the DAP server: {err}");
            return;
        }
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(err) => {
                eprintln!("luadebug: can't start the DAP server: {err}");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let _ = stream.set_nodelay(true);
            let (reader, writer) = stream.into_split();
            let mut session = DapSession::new_attached(reader, writer, engine);
            if let Err(err) = session.run_loop().await {
                eprintln!("luadebug: debug session ended with an error: {err:?}");
            }
            engine = match session.into_engine() {
                Some(e) => e,
                None => return,
            };
        }
    });
}

/*
 * "a.b.c" -> the table holding c, and "c"; None while a table on the way isn't there yet
 */
fn global_slot(lua: &Lua, name: &str) -> Result<Option<(Table, String)>, String> {
    let mut parts: Vec<&str> = name.split('.').collect();
    let key = parts.pop().unwrap_or_default();
    if key.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("luadebug: '{name}' isn't a global name"));
    }
    let mut table = lua.globals();
    for part in parts {
        table = match table.get::<Value>(part) {
            Ok(Value::Table(t)) => t,
            Ok(Value::Nil) => return Ok(None),
            _ => return Err(format!("luadebug: '{name}': {part} isn't a table")),
        };
    }
    Ok(Some((table, key.to_string())))
}

fn global_function(lua: &Lua, name: &str) -> Result<Function, String> {
    let slot = global_slot(lua, name)?;
    match slot.map(|(table, key)| table.get::<Value>(key.as_str())) {
        Some(Ok(Value::Function(f))) => Ok(f),
        _ => Err(format!("luadebug: '{name}' isn't a function")),
    }
}

/*
 * Wraps the functions the host calls as its xpcall message handler, so errors stop in
 * the debugger while the failing frame is still on the stack; the originals run after.
 * One that isn't defined yet is fine: the hook retries until it is.
 */
fn ensure_error_handlers(lua: &Lua, state: &RefCell<HostState>) -> Result<(), String> {
    let count = state.borrow().error_handlers.len();
    for i in 0..count {
        let (name, wrapper) = state.borrow().error_handlers[i].clone();
        let Some((table, key)) = global_slot(lua, &name)? else {
            continue;
        };
        let current = match table.get::<Value>(key.as_str()) {
            Ok(Value::Function(f)) => f,
            Ok(Value::Nil) => continue,
            _ => return Err(format!("luadebug: errorHandler '{name}' isn't a function")),
        };
        if wrapper.map(|w| w.to_pointer()) == Some(current.to_pointer()) {
            continue;
        }
        let handling = Rc::clone(&state.borrow().handling);
        let wrapper = lua
            .create_function(move |lua, args: MultiValue| {
                let outer = !handling.replace(true);
                if outer {
                    if let Some(err) = args.iter().next() {
                        stop_on_exception(lua, err, 0);
                    }
                }
                let result = current.call::<MultiValue>(args);
                if outer {
                    handling.set(false);
                }
                result
            })
            .map_err(|e| e.to_string())?;
        table
            .set(key, wrapper.clone())
            .map_err(|e| format!("luadebug: can't wrap '{name}': {e}"))?;
        state.borrow_mut().error_handlers[i].1 = Some(wrapper);
    }
    Ok(())
}

/*
 * Stops with reason "exception" while the failing frame is still on the stack
 */
fn stop_on_exception(lua: &Lua, err: &Value, skip: usize) {
    let Some(engine) = ENGINE.get() else {
        return;
    };
    if !engine.control.client_attached.load(Ordering::Relaxed)
        || engine.control.terminate_requested.load(Ordering::Relaxed)
        || !*engine.exception_enabled.lock()
    {
        return;
    }
    let message = Evaluator::format_lua_error_value(lua, err);
    let level = position_frame_level(lua, &message).unwrap_or_else(|| error_frame_level(lua, skip));
    let depth = *engine.stack_depth.lock();
    LuaRunner::pause_and_wait(
        lua,
        "exception",
        Some(message),
        depth,
        level,
        &engine.step_mode,
        &engine.cmd_rx,
        &engine.event_tx,
    );
}

/*
 * Lua prefixes error messages with where they happened ("main.lua:7: ..."), so the
 * failing frame is the innermost one at that source and line
 */
fn position_frame_level(lua: &Lua, message: &str) -> Option<usize> {
    let (source, line) = error_position(message)?;
    (1..64)
        .map_while(|level| {
            lua.inspect_stack(level, |d| {
                let short_src = d.source().short_src.map(|s| s.into_owned()).unwrap_or_default();
                (short_src, d.current_line())
            })
            .map(|frame| (level, frame))
        })
        .find(|(_, (short_src, current))| *current == Some(line) && *short_src == source)
        .map(|(level, _)| level)
}

/*
 * "<short_src>:<line>: <message>" -> (short_src, line)
 */
fn error_position(message: &str) -> Option<(String, usize)> {
    let bytes = message.as_bytes();
    let mut i = 0;
    while let Some(off) = message[i..].find(':') {
        let colon = i + off;
        let digits = bytes[colon + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 && bytes.get(colon + 1 + digits) == Some(&b':') {
            let line = message[colon + 1..colon + 1 + digits].parse().ok()?;
            return Some((message[..colon].to_string(), line));
        }
        i = colon + 1;
    }
    None
}

/*
 * Fallback for errors with no position (error(msg, 0), non-string error values).
 * Skips C frames and "="-named chunks, which aren't files (a host's built-in scripts).
 */
fn error_frame_level(lua: &Lua, mut skip: usize) -> usize {
    for level in 0..64 {
        let frame = lua.inspect_stack(level, |d| {
            let source = d.source().source.map(|s| s.into_owned()).unwrap_or_default();
            (d.current_line().is_some(), source)
        });
        match frame {
            Some((true, source)) if !source.starts_with('=') => {
                if skip == 0 {
                    return level;
                }
                skip -= 1;
            }
            Some(_) => continue,
            None => break,
        }
    }
    0
}
