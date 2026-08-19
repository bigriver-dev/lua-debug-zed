use crate::engine::breakpoints::BreakpointRegistry;
use crate::engine::evaluator::{DapVariable, Evaluator, TableRegistry};
use crossbeam_channel::{Receiver, Sender};
use mlua::debug::{Debug as LuaDebug, DebugEvent};
use mlua::{HookTriggers, Lua, Result, Table, Value, VmState};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

pub enum ExecutionCommand {
    Continue,
    StepOver,
    StepIn,
    StepOut,
    GetStackTrace {
        responder: Sender<Vec<DapStackFrame>>,
    },
    GetVariables {
        frame_id: usize,
        responder: Sender<Vec<DapVariable>>,
    },
    GetGlobals {
        responder: Sender<Vec<DapVariable>>,
    },
    GetTableContents {
        table_ref: usize,
        responder: Sender<Vec<DapVariable>>,
    },
    Evaluate {
        frame_id: usize,
        expression: String,
        responder: Sender<std::result::Result<DapVariable, String>>,
    },
}

#[derive(Debug, Clone)]
pub enum RunnerEvent {
    Stopped {
        reason: &'static str,
        message: Option<String>,
    },
    Terminated {
        error: Option<String>,
    },
    Output(String),
}

#[derive(Debug, Clone)]
pub struct DapStackFrame {
    pub id: usize,
    pub name: String,
    pub source_path: String,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    None,
    StepOver { target_depth: usize },
    StepIn,
    StepOut { target_depth: usize },
}

pub struct LuaRunner {
    lua: Lua,
    breakpoints: Arc<Mutex<BreakpointRegistry>>,
}

impl LuaRunner {
    /*
     * Instantiates the Lua runtime
     */
    pub fn new(breakpoints: Arc<Mutex<BreakpointRegistry>>) -> Result<Self> {
        // let lua = Lua::new();
        // required! The created Lua state will not have safety guarantees and will allow to load C modules.
        // https://docs.rs/mlua/latest/mlua/struct.Lua.html#method.unsafe_new
        let lua = unsafe { Lua::unsafe_new() };
        Ok(Self { lua, breakpoints })
    }

    /*
     * Configures package.cpath for .dll/.so loading, attaches lua.set_hook(), and begins script execution.
     */
    pub fn execute_script(
        &self,
        script_path: &Path,
        c_dll_dir: Option<&Path>,
        preload_dirs: &[PathBuf],
        cmd_receiver: crossbeam_channel::Receiver<ExecutionCommand>,
        event_sender: UnboundedSender<RunnerEvent>,
    ) -> Result<()> {
        let hook_event_sender = event_sender.clone();
        let result = (|| -> Result<()> {
            // configure package.cpath so Lua can load compiled C/C++ dll/so extensions
            if let Some(dll_dir) = c_dll_dir {
                let globals = self.lua.globals();
                let package: Table = globals.get("package")?;
                let current_cpath: String = package.get("cpath")?;
                let new_cpath = format!(
                    "{};{}/?.dll;{}/?/init.dll;{}/?.so",
                    current_cpath,
                    dll_dir.display(),
                    dll_dir.display(),
                    dll_dir.display()
                );
                package.set("cpath", new_cpath)?;
            }

            // Stepping state tracked across hook invocations
            let step_mode = Arc::new(Mutex::new(StepMode::None));
            let stack_depth = Arc::new(Mutex::new(0usize));

            // Clone what the xpcall error handler (below) needs before the
            // hook closure moves the originals.
            let error_cmd_receiver = cmd_receiver.clone();
            let error_step_mode = Arc::clone(&step_mode);
            let error_stack_depth = Arc::clone(&stack_depth);
            let error_event_sender = event_sender.clone();

            // Register safe Rust debug hook
            let bps = Arc::clone(&self.breakpoints);
            self.lua.set_hook(
                HookTriggers {
                    every_line: true,
                    on_calls: true,
                    on_returns: true,
                    ..Default::default()
                },
                move |lua, debug| {
                    Self::on_debug_hook(
                        lua,
                        &debug,
                        &bps,
                        &step_mode,
                        &stack_depth,
                        &cmd_receiver,
                        &hook_event_sender,
                    )?;
                    Ok(VmState::Continue)
                },
            )?;

            // auto-load dll/so or .lua from configured folder
            for dir in preload_dirs {
                Self::preload_directory(&self.lua, dir, &event_sender);
            }

            // Load and execute target Lua script inside this process
            let code = std::fs::read_to_string(script_path)
                .map_err(|e| mlua::Error::ExternalError(Arc::new(e)))?;

            let chunk_fn = self
                .lua
                .load(&code)
                .set_name(script_path.to_string_lossy())
                .into_function()?;

            let error_handler = self.lua.create_function(move |lua, err: Value| {
                let message = Evaluator::format_lua_error_value(lua, &err);
                let base_level = Evaluator::find_error_frame_level(lua);
                let depth = *error_stack_depth.lock();
                Self::pause_and_wait(
                    lua,
                    "exception",
                    Some(message),
                    depth,
                    base_level,
                    &error_step_mode,
                    &error_cmd_receiver,
                    &error_event_sender,
                );
                Ok(err)
            })?;

            let xpcall_fn: mlua::Function = self.lua.globals().get("xpcall")?;
            // Discard (ok, err) deliberately: a caught failure was already found
            let _: (bool, Value) = xpcall_fn.call((chunk_fn, error_handler))?;

            Ok(())
        })();

        // prevent hangs or other weird stuff by terminate signal
        let error_message = result.as_ref().err().map(|e| e.to_string());
        let _ = event_sender.send(RunnerEvent::Terminated {
            error: error_message,
        });

        result
    }

    /*
     * Scans a folder (non-recursively) for .lua and .dll/.so files;
     * loads each into a global named after its filename
     */
    fn preload_directory(lua: &Lua, dir: &Path, event_sender: &UnboundedSender<RunnerEvent>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                let _ = event_sender.send(RunnerEvent::Output(format!(
                    "Could not read preload folder {}: {}",
                    dir.display(),
                    err
                )));
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            let result: mlua::Result<Value> = match ext.as_str() {
                "lua" => std::fs::read_to_string(&path)
                    .map_err(|e| mlua::Error::ExternalError(Arc::new(e)))
                    .and_then(|code| lua.load(&code).set_name(path.to_string_lossy()).eval()),
                "dll" | "so" => {
                    // why couldn't windows & linux agree on their "/" choices
                    let snippet = format!(
                        "return package.loadlib([[{}]], \"luaopen_{}\")()",
                        path.display(),
                        stem
                    );
                    lua.load(&snippet).eval()
                }
                _ => continue,
            };

            match result {
                Ok(Value::Nil) => {}
                Ok(value) => {
                    if let Err(err) = lua.globals().set(stem.clone(), value) {
                        let _ = event_sender.send(RunnerEvent::Output(format!(
                            "Failed to bind preloaded module '{}': {}",
                            stem, err
                        )));
                    }
                }
                Err(err) => {
                    let _ = event_sender.send(RunnerEvent::Output(format!(
                        "Failed to preload {}: {}",
                        path.display(),
                        err
                    )));
                }
            }
        }
    }

    /*
     * Internal callback triggered on line/call/return events by mlua. Checks breakpoint matches, notifies the session via event channels, and waits on execution commands
     */
    pub fn on_debug_hook(
        _lua: &Lua,
        debug: &LuaDebug,
        breakpoints: &Arc<Mutex<BreakpointRegistry>>,
        step_mode: &Arc<Mutex<StepMode>>,
        stack_depth: &Arc<Mutex<usize>>,
        cmd_receiver: &Receiver<ExecutionCommand>,
        event_sender: &UnboundedSender<RunnerEvent>,
    ) -> Result<()> {
        let event = debug.event();

        // internal call stack depth counter
        let mut depth = stack_depth.lock();
        match event {
            DebugEvent::Call | DebugEvent::TailCall => {
                *depth += 1;
                return Ok(());
            }
            DebugEvent::Ret => {
                if *depth > 0 {
                    *depth -= 1;
                }
                return Ok(());
            }
            DebugEvent::Line => {}
            // base case for events to ignore (maybe log these?)
            _ => return Ok(()),
        }

        let current_depth = *depth;
        drop(depth);

        // extract source path and line number.
        let line = debug.current_line().unwrap_or(0);
        let src_str = match debug.source().source {
            Some(s) => s,
            None => return Ok(()),
        };
        let clean_path_str = src_str.strip_prefix('@').unwrap_or(&src_str);
        let src_path = PathBuf::from(clean_path_str);

        // evaluate breakpoint or step conditions
        let is_breakpoint_hit = {
            let registry = breakpoints.lock();
            registry.is_breakpoint(&src_path, line)
        };

        let current_step = *step_mode.lock();
        let should_step_pause = match current_step {
            StepMode::None => false,
            StepMode::StepIn => true,
            StepMode::StepOver { target_depth } => current_depth <= target_depth,
            StepMode::StepOut { target_depth } => current_depth < target_depth,
        };

        if !is_breakpoint_hit && !should_step_pause {
            return Ok(());
        }

        // reset step mode upon hitting a pause condition
        *step_mode.lock() = StepMode::None;

        let reason = if is_breakpoint_hit {
            "breakpoint"
        } else {
            "step"
        };

        Self::pause_and_wait(
            _lua,
            reason,
            None,
            current_depth,
            0,
            step_mode,
            cmd_receiver,
            event_sender,
        );

        Ok(())
    }

    /*
     * pauses running script
     * sends a "stopped" event with the given reason
     * blocks the calling thread until client sends
     * (Continue/StepOver/StepIn/StepOut)
     */
    fn pause_and_wait(
        lua: &Lua,
        reason: &'static str,
        message: Option<String>,
        current_depth: usize,
        base_level: usize,
        step_mode: &Mutex<StepMode>,
        cmd_receiver: &Receiver<ExecutionCommand>,
        event_sender: &UnboundedSender<RunnerEvent>,
    ) {
        let mut table_registry = TableRegistry::new();
        let _ = event_sender.send(RunnerEvent::Stopped { reason, message });

        loop {
            match cmd_receiver.recv() {
                Ok(ExecutionCommand::Continue) => {
                    *step_mode.lock() = StepMode::None;
                    break;
                }
                Ok(ExecutionCommand::StepOver) => {
                    *step_mode.lock() = StepMode::StepOver {
                        target_depth: current_depth,
                    };
                    break;
                }
                Ok(ExecutionCommand::StepIn) => {
                    *step_mode.lock() = StepMode::StepIn;
                    break;
                }
                Ok(ExecutionCommand::StepOut) => {
                    *step_mode.lock() = StepMode::StepOut {
                        target_depth: current_depth,
                    };
                    break;
                }
                Ok(ExecutionCommand::GetStackTrace { responder }) => {
                    let frames = Self::capture_frames(lua, base_level);
                    let _ = responder.send(frames);
                    // Stay paused, wait for the next command.
                }
                Ok(ExecutionCommand::GetVariables {
                    frame_id,
                    responder,
                }) => {
                    let vars = Evaluator::get_frame_variables(
                        lua,
                        frame_id,
                        base_level,
                        &mut table_registry,
                    )
                    .unwrap_or_default();
                    let _ = responder.send(vars);
                }
                Ok(ExecutionCommand::GetGlobals { responder }) => {
                    let vars = Evaluator::get_globals(lua, &mut table_registry).unwrap_or_default();
                    let _ = responder.send(vars);
                }
                Ok(ExecutionCommand::GetTableContents {
                    table_ref,
                    responder,
                }) => {
                    let vars = Evaluator::get_table_contents(&mut table_registry, table_ref);
                    let _ = responder.send(vars);
                }
                Ok(ExecutionCommand::Evaluate {
                    frame_id,
                    expression,
                    responder,
                }) => {
                    let result = Evaluator::evaluate_expression(
                        lua,
                        frame_id,
                        base_level,
                        &expression,
                        &mut table_registry,
                    )
                    .map_err(|e| e.to_string());
                    let _ = responder.send(result);
                }
                Err(_) => {
                    // exterminate! exterminate them!
                    break;
                }
            }
        }
    }

    /*
     * walk the Lua call stack
     */
    pub fn capture_frames(lua: &Lua, base_level: usize) -> Vec<DapStackFrame> {
        let mut frames = Vec::new();
        let mut level = base_level;

        while let Some(frame) = lua.inspect_stack(level, |info| {
            let name = info
                .names()
                .name
                .map(|s| s.into_owned())
                .unwrap_or_else(|| "<anonymous>".to_string());
            let source_path = info
                .source()
                .source
                .map(|s| s.into_owned())
                .unwrap_or_default();
            let line = info.current_line().unwrap_or(0);

            DapStackFrame {
                id: level - base_level,
                name,
                source_path,
                line,
            }
        }) {
            frames.push(frame);
            level += 1;
        }

        frames
    }
}
