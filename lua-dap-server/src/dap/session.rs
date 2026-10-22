/*
 * the main DAP state machine
 * Processes client requests and routes commands to target process
 *
 * initialize
 * launch
 * attach
 * setBreakpoints
 * stackTrace
 * scopes
 * variables
 */

use crate::dap::protocol::*;
use crate::dap::proxy::{self, LaunchedHost};
use crate::dap::transport::DapTransport;
use crate::engine::breakpoints::{
    BreakpointRegistry, FunctionBreakpointRegistry, HitCondition, SourceBp,
};
use crate::engine::runner::{ExecutionCommand, LuaRunner, RunControl, RunnerEvent, ScriptEnv};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// Wire-message trace for debugging; a no-op in release builds so shipped
// binaries never write lua-dap-server-trace.log next to the user's project.
fn trace(direction: &str, msg: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("lua-dap-server-trace.log")
    {
        let _ = writeln!(f, "{direction}: {msg}");
    }
}

struct PendingLaunch {
    program_path: PathBuf,
    dll_dir: Option<PathBuf>,
    preload_dirs: Vec<PathBuf>,
    env: ScriptEnv,
    stop_on_entry: bool,
    cmd_rx: Receiver<ExecutionCommand>,
    event_tx: UnboundedSender<RunnerEvent>,
}

/*
 * Engine handles for a Lua state this process didn't create
 */
pub struct AttachedEngine {
    pub breakpoints: Arc<Mutex<BreakpointRegistry>>,
    pub function_breakpoints: Arc<Mutex<FunctionBreakpointRegistry>>,
    pub exception_breakpoints_enabled: Arc<Mutex<bool>>,
    pub control: Arc<RunControl>,
    pub cmd_sender: Sender<ExecutionCommand>,
    pub event_receiver: UnboundedReceiver<RunnerEvent>,
    pub on_configured: Option<Sender<()>>,
    // each client's attach arguments (its debug.json entry), for the host to apply
    pub on_attach: Option<Sender<serde_json::Value>>,
}

pub struct DapSession<R, W> {
    transport: DapTransport<R, W>,
    seq: i64,
    breakpoints: Arc<Mutex<BreakpointRegistry>>,
    function_breakpoints: Arc<Mutex<FunctionBreakpointRegistry>>,
    exception_breakpoints_enabled: Arc<Mutex<bool>>,
    cmd_sender: Option<Sender<ExecutionCommand>>,
    event_receiver: Option<UnboundedReceiver<RunnerEvent>>,
    pending_launch: Option<PendingLaunch>,
    last_exception: Option<String>,
    control: Arc<RunControl>,
    paused: bool,
    attached: bool,
    on_configured: Option<Sender<()>>,
    on_attach: Option<Sender<serde_json::Value>>,
    launched_host: Option<LaunchedHost>,
}

impl<R, W> DapSession<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            transport: DapTransport::new(reader, writer),
            seq: 1,
            breakpoints: Arc::new(Mutex::new(BreakpointRegistry::new())),
            function_breakpoints: Arc::new(Mutex::new(FunctionBreakpointRegistry::new())),
            exception_breakpoints_enabled: Arc::new(Mutex::new(true)),
            cmd_sender: None,
            event_receiver: None,
            pending_launch: None,
            last_exception: None,
            control: Arc::new(RunControl::default()),
            paused: false,
            attached: false,
            on_configured: None,
            on_attach: None,
            launched_host: None,
        }
    }

    pub fn new_attached(reader: R, writer: W, engine: AttachedEngine) -> Self {
        Self {
            transport: DapTransport::new(reader, writer),
            seq: 1,
            breakpoints: engine.breakpoints,
            function_breakpoints: engine.function_breakpoints,
            exception_breakpoints_enabled: engine.exception_breakpoints_enabled,
            cmd_sender: Some(engine.cmd_sender),
            event_receiver: Some(engine.event_receiver),
            pending_launch: None,
            last_exception: None,
            control: engine.control,
            paused: false,
            attached: true,
            on_configured: engine.on_configured,
            on_attach: engine.on_attach,
            launched_host: None,
        }
    }

    /*
     * Gives the handles back once an attached session ends
     */
    pub fn into_engine(self) -> Option<AttachedEngine> {
        Some(AttachedEngine {
            breakpoints: self.breakpoints,
            function_breakpoints: self.function_breakpoints,
            exception_breakpoints_enabled: self.exception_breakpoints_enabled,
            control: self.control,
            cmd_sender: self.cmd_sender?,
            event_receiver: self.event_receiver?,
            on_configured: self.on_configured,
            on_attach: self.on_attach,
        })
    }

    /*
     * Attached mode: asks the host to end itself or release it if its hung
     */
    fn request_host_exit(&mut self) {
        self.control
            .terminate_requested
            .store(true, Ordering::Relaxed);
        if self.paused {
            self.paused = false;
            if let Some(ref tx) = self.cmd_sender {
                let _ = tx.send(ExecutionCommand::Continue);
            }
        }
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            std::process::exit(0);
        });
    }

    fn detach(&mut self) {
        self.control.client_attached.store(false, Ordering::Relaxed);
        self.control.pause_requested.store(false, Ordering::Relaxed);
        self.breakpoints.lock().clear_all();
        self.function_breakpoints.lock().set_breakpoints(Vec::new());
        if self.paused {
            self.paused = false;
            if let Some(ref tx) = self.cmd_sender {
                let _ = tx.send(ExecutionCommand::Continue);
            }
        }
    }

    /*
     * loop processing incoming messages from DapTransport alongside engine events
     */
    pub async fn run_loop(&mut self) -> Result<()> {
        loop {
            let raw_msg = tokio::select! {
                msg = self.transport.read_msg() => match msg? {
                    Some(m) => m,
                    None => break, // EOF
                },
                event = Self::recv_runner_event(&mut self.event_receiver) => {
                    match event {
                        Some(event) => self.handle_runner_event(event).await?,
                        None => self.event_receiver = None,
                    }
                    continue;
                },
            };

            trace("IN", &raw_msg);

            let message: ProtocolMessage = match serde_json::from_str(&raw_msg) {
                Ok(msg) => msg,
                Err(err) => {
                    eprintln!("Failed to parse DAP message: {}", err);
                    continue;
                }
            };

            if let ProtocolMessage::Request(req) = message {
                let should_exit = self.handle_request(req).await?;
                if should_exit {
                    break;
                }
            }

            if let Some(host) = self.launched_host.take() {
                return proxy::relay(&mut self.transport, host, self.seq).await;
            }
        }

        if self.attached {
            self.detach();
        }

        Ok(())
    }

    /*
     * wait on the runner event channel when one exists
     * (before, this was drain all events which lead to a race condition)
     */
    async fn recv_runner_event(
        rx: &mut Option<UnboundedReceiver<RunnerEvent>>,
    ) -> Option<RunnerEvent> {
        match rx {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    }

    /*
     * figure out what to do with each event
     */
    async fn handle_runner_event(&mut self, event: RunnerEvent) -> Result<()> {
        match event {
            RunnerEvent::Stopped { reason, message } => {
                self.paused = true;
                self.control.pause_requested.store(false, Ordering::Relaxed);
                self.last_exception = message.clone();
                if reason == "exception" {
                    if let Some(ref text) = message {
                        let body = json!({ "category": "stderr", "output": format!("Uncaught error: {}\n", text) });
                        self.send_event(Event::new("output", Some(body))).await?;
                    }
                }
                self.send_event(Event::stopped_with_text(reason, 1, message))
                    .await?;
            }
            RunnerEvent::Terminated { error } => {
                if let Some(message) = error {
                    let body = json!({ "category": "stderr", "output": format!("{}\n", message) });
                    self.send_event(Event::new("output", Some(body))).await?;
                }
                self.send_event(Event::terminated()).await?;
            }
            RunnerEvent::Output(message) => {
                let body = json!({ "category": "stdout", "output": message });
                self.send_event(Event::new("output", Some(body))).await?;
            }
        }
        Ok(())
    }

    /*
     * Dispatcher routing requests to internal handlers
     */
    pub async fn handle_request(&mut self, req: Request) -> Result<bool> {
        let request_seq = req.seq;
        let command = req.command.clone();

        match command.as_str() {
            "initialize" => {
                let capabilities = json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsEvaluateForHovers": true,
                    "supportsConditionalBreakpoints": true,
                    "supportsExceptionInfoRequest": true,
                    "supportsSetVariable": true,
                    "supportsLogPoints": true,
                    "supportsHitConditionalBreakpoints": true,
                    "supportsTerminateRequest": true,
                    "exceptionBreakpointFilters": [
                        {
                            "filter": "uncaught",
                            "label": "Uncaught Exceptions",
                            "default": true
                        }
                    ],
                });
                self.send_response(Response::success(request_seq, &command, Some(capabilities)))
                    .await?;
            }

            "attach" if self.attached => {
                if let Some(tx) = &self.on_attach {
                    let _ = tx.send(req.arguments.clone().unwrap_or_default());
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
                self.send_event(Event::initialized()).await?;
            }

            "launch" if self.attached => {
                self.send_response(Response::error(
                    request_seq,
                    &command,
                    "This debugger is running inside an existing Lua host; use \"request\": \"attach\"",
                ))
                .await?;
            }

            "launch" => {
                let raw_args = req.arguments.unwrap_or_default();
                let args: LaunchRequestArguments = serde_json::from_value(raw_args.clone())?;

                if args
                    .runtime_executable
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty())
                {
                    let mut console = Vec::new();
                    let launched = proxy::launch(&args, &raw_args, &mut console).await;
                    for (category, text) in console {
                        let body = json!({ "category": category, "output": text });
                        self.send_event(Event::new("output", Some(body))).await?;
                    }
                    match launched {
                        Ok(host) => {
                            self.launched_host = Some(host);
                            self.send_response(Response::success(request_seq, &command, None))
                                .await?;
                            self.send_event(Event::initialized()).await?;
                        }
                        Err(err) => {
                            self.send_response(Response::error(
                                request_seq,
                                &command,
                                format!("{err:#}"),
                            ))
                            .await?;
                        }
                    }
                    return Ok(false);
                }

                if args.program.trim().is_empty() {
                    self.send_response(Response::error(
                        request_seq,
                        &command,
                        "\"program\" is required: the Lua file to debug (or set \"runtimeExecutable\"                          to launch a program that loads lua-debug-module)",
                    ))
                    .await?;
                    return Ok(false);
                }

                let (cmd_tx, cmd_rx) = unbounded();
                let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

                self.cmd_sender = Some(cmd_tx);
                self.event_receiver = Some(event_rx);

                self.pending_launch = Some(PendingLaunch {
                    program_path: PathBuf::from(args.program),
                    dll_dir: args.c_dll_dir.map(PathBuf::from),
                    stop_on_entry: args.stop_on_entry,
                    preload_dirs: args.preload_paths.into_iter().map(PathBuf::from).collect(),
                    env: ScriptEnv {
                        cwd: args.cwd.map(PathBuf::from),
                        args: args.args,
                        package_path: args
                            .package_path
                            .map(PathList::into_vec)
                            .unwrap_or_default(),
                        package_cpath: args
                            .package_cpath
                            .map(PathList::into_vec)
                            .unwrap_or_default(),
                    },
                    cmd_rx,
                    event_tx,
                });

                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
                self.send_event(Event::initialized()).await?;
            }

            "setBreakpoints" => {
                let args: SetBreakpointsArguments =
                    serde_json::from_value(req.arguments.unwrap_or_default())?;

                let mut verified_breakpoints = Vec::new();

                if let Some(path_str) = args.source.path {
                    let path = PathBuf::from(&path_str);

                    // dump all breakpoints. Clients can send "" for an unused field.
                    let non_blank = |s: &Option<String>| {
                        s.as_deref()
                            .filter(|t| !t.trim().is_empty())
                            .map(String::from)
                    };
                    // a breakpoint whose hit condition doesn't parse is left unarmed (reported
                    // unverified below) rather than silently stopping on every hit
                    let entries: Vec<SourceBp> = args
                        .breakpoints
                        .iter()
                        .filter_map(|bp| {
                            let hit_condition = match non_blank(&bp.hit_condition) {
                                Some(text) => Some(HitCondition::parse(&text)?),
                                None => None,
                            };
                            Some(SourceBp {
                                line: bp.line,
                                condition: non_blank(&bp.condition),
                                hit_condition,
                                log_message: non_blank(&bp.log_message),
                            })
                        })
                        .collect();

                    // set_breakpoints based on line numbers
                    {
                        let mut registry = self.breakpoints.lock();
                        registry.set_breakpoints(path.clone(), entries);
                    }

                    // DAP response verification payloads
                    for bp in args.breakpoints {
                        let bad_hit_condition = non_blank(&bp.hit_condition)
                            .filter(|t| HitCondition::parse(t).is_none());
                        verified_breakpoints.push(Breakpoint {
                            id: Some(bp.line),
                            verified: bad_hit_condition.is_none(),
                            message: bad_hit_condition.map(|t| {
                                format!("Invalid hit condition '{t}' (expected e.g. 5, >= 5, % 3)")
                            }),
                            source: Some(Source {
                                name: args.source.name.clone(),
                                path: Some(path_str.clone()),
                            }),
                            line: Some(bp.line),
                        });
                    }
                }

                let body = json!({ "breakpoints": verified_breakpoints });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "configurationDone" if self.attached => {
                self.control.client_attached.store(true, Ordering::Relaxed);
                if let Some(ready) = self.on_configured.take() {
                    let _ = ready.send(());
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "configurationDone" => {
                if let Some(pending) = self.pending_launch.take() {
                    let bps = Arc::clone(&self.breakpoints);
                    let func_bps = Arc::clone(&self.function_breakpoints);
                    let exception_enabled = Arc::clone(&self.exception_breakpoints_enabled);
                    let control = Arc::clone(&self.control);
                    std::thread::spawn(move || {
                        match LuaRunner::new(bps, func_bps, exception_enabled, control) {
                            Ok(runner) => {
                                let _ = runner.execute_script(
                                    &pending.program_path,
                                    pending.stop_on_entry,
                                    pending.dll_dir.as_deref(),
                                    &pending.preload_dirs,
                                    pending.env,
                                    pending.cmd_rx,
                                    pending.event_tx,
                                );
                            }
                            Err(err) => {
                                let _ = pending.event_tx.send(RunnerEvent::Terminated {
                                    error: Some(err.to_string()),
                                });
                            }
                        }
                    });
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "setExceptionBreakpoints" => {
                let args = req.arguments.unwrap_or_default();
                let filters: Vec<String> = args
                    .get("filters")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                *self.exception_breakpoints_enabled.lock() =
                    filters.iter().any(|f| f == "uncaught");
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "exceptionInfo" => {
                let description = self
                    .last_exception
                    .clone()
                    .unwrap_or_else(|| "Unknown error".to_string());
                let body = json!({
                    "exceptionId": "lua-runtime-error",
                    "description": description,
                    "breakMode": "unhandled",
                });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "setFunctionBreakpoints" => {
                let args: SetFunctionBreakpointsArguments =
                    serde_json::from_value(req.arguments.unwrap_or_default())?;

                let entries: Vec<(String, Option<String>)> = args
                    .breakpoints
                    .iter()
                    .map(|bp| (bp.name.clone(), bp.condition.clone()))
                    .collect();

                let verified = vec![
                    Breakpoint {
                        id: None,
                        verified: true,
                        message: None,
                        source: None,
                        line: None,
                    };
                    args.breakpoints.len()
                ];

                self.function_breakpoints.lock().set_breakpoints(entries);

                let body = json!({ "breakpoints": verified });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "threads" => {
                let body = json!({
                    "threads": [
                        { "id": 1, "name": "Main Lua Thread" }
                    ]
                });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "stackTrace" => {
                let mut frames = Vec::new();

                if let Some(ref tx) = self.cmd_sender {
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);

                    // send request to lua thread
                    if tx
                        .send(ExecutionCommand::GetStackTrace { responder: resp_tx })
                        .is_ok()
                    {
                        // wait for response from lua thread
                        if let Ok(captured_frames) = resp_rx.recv() {
                            frames = captured_frames
                                .into_iter()
                                .map(|f| {
                                    let file_name = std::path::Path::new(&f.source_path)
                                        .file_name()
                                        .map(|s| s.to_string_lossy().into_owned());

                                    json!({
                                        "id": f.id,
                                        "name": f.name,
                                        "source": {
                                            "name": file_name,
                                            "path": f.source_path,
                                        },
                                        "line": f.line,
                                        "column": 1
                                    })
                                })
                                .collect();
                        }
                    }
                }

                let body = json!({
                    "stackFrames": frames,
                    "totalFrames": frames.len()
                });

                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "scopes" => {
                let args = req.arguments.unwrap_or_default();
                let frame_id = args.get("frameId").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                let scopes = vec![
                    Scope {
                        name: "Locals".to_string(),
                        variables_reference: 1000 + frame_id,
                        expensive: false,
                    },
                    Scope {
                        name: "Upvalues".to_string(),
                        variables_reference: 1500 + frame_id,
                        expensive: false,
                    },
                    Scope {
                        name: "Globals".to_string(),
                        variables_reference: 2000 + frame_id,
                        expensive: true,
                    },
                ];

                let body = json!({ "scopes": scopes });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "variables" => {
                let args = req.arguments.unwrap_or_default();
                let var_ref = args
                    .get("variablesReference")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;

                let mut vars = Vec::new();
                if let Some(ref tx) = self.cmd_sender {
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                    let sent = if var_ref >= 10000 {
                        tx.send(ExecutionCommand::GetTableContents {
                            table_ref: var_ref,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else if var_ref >= 2000 {
                        tx.send(ExecutionCommand::GetGlobals { responder: resp_tx })
                            .is_ok()
                    } else if var_ref >= 1500 {
                        tx.send(ExecutionCommand::GetUpvalues {
                            frame_id: var_ref - 1500,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else if var_ref >= 1000 {
                        tx.send(ExecutionCommand::GetVariables {
                            frame_id: var_ref - 1000,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else {
                        false
                    };

                    if sent {
                        if let Ok(captured_vars) = resp_rx.recv() {
                            vars = captured_vars
                                .into_iter()
                                .map(|v| Variable {
                                    name: v.name,
                                    value: v.value,
                                    var_type: Some(v.var_type),
                                    variables_reference: v.variables_reference,
                                })
                                .collect();
                        }
                    }
                }

                let body = json!({ "variables": vars });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "setVariable" => {
                let args = req.arguments.unwrap_or_default();
                let var_ref = args
                    .get("variablesReference")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let value_expr = args
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let mut set_result = None;
                if let Some(ref tx) = self.cmd_sender {
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                    let sent = if var_ref >= 10000 {
                        tx.send(ExecutionCommand::SetTableValue {
                            table_ref: var_ref,
                            name,
                            value_expr,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else if var_ref >= 2000 {
                        tx.send(ExecutionCommand::SetGlobal {
                            name,
                            value_expr,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else if var_ref >= 1500 {
                        tx.send(ExecutionCommand::SetUpvalue {
                            frame_id: var_ref - 1500,
                            name,
                            value_expr,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else if var_ref >= 1000 {
                        tx.send(ExecutionCommand::SetLocal {
                            frame_id: var_ref - 1000,
                            name,
                            value_expr,
                            responder: resp_tx,
                        })
                        .is_ok()
                    } else {
                        false
                    };

                    if sent {
                        set_result = resp_rx.recv().ok();
                    }
                }

                match set_result {
                    Some(Ok(var)) => {
                        let body = json!({
                            "value": var.value,
                            "type": var.var_type,
                            "variablesReference": var.variables_reference,
                        });
                        self.send_response(Response::success(request_seq, &command, Some(body)))
                            .await?;
                    }
                    Some(Err(err)) => {
                        self.send_response(Response::error(request_seq, &command, err))
                            .await?;
                    }
                    None => {
                        self.send_response(Response::error(
                            request_seq,
                            &command,
                            "No active Lua execution to set variable against",
                        ))
                        .await?;
                    }
                }
            }

            "evaluate" => {
                let args: EvaluateArguments =
                    serde_json::from_value(req.arguments.unwrap_or_default())?;

                let frame_id = args.frame_id.unwrap_or(0);

                let eval_result = if let Some(ref tx) = self.cmd_sender {
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                    let sent = tx.send(ExecutionCommand::Evaluate {
                        frame_id,
                        expression: args.expression.clone(),
                        responder: resp_tx,
                    });
                    sent.ok().and_then(|_| resp_rx.recv().ok())
                } else {
                    None
                };

                match eval_result {
                    Some(Ok(result_var)) => {
                        let body = json!({
                            "result": result_var.value,
                            "type": result_var.var_type,
                            "variablesReference": result_var.variables_reference,
                        });
                        self.send_response(Response::success(request_seq, &command, Some(body)))
                            .await?;
                    }
                    Some(Err(err)) => {
                        self.send_response(Response::error(request_seq, &command, err.to_string()))
                            .await?;
                    }
                    None => {
                        self.send_response(Response::error(
                            request_seq,
                            &command,
                            "No active Lua execution to evaluate against",
                        ))
                        .await?;
                    }
                }
            }

            "pause" => {
                if !self.paused && self.cmd_sender.is_some() {
                    self.control.pause_requested.store(true, Ordering::Relaxed);
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            // attached: ending the session means ending the host process
            "terminate" if self.attached => {
                self.request_host_exit();
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
                self.send_event(Event::terminated()).await?;
            }

            "terminate" => {
                if self.pending_launch.take().is_some() || self.cmd_sender.is_none() {
                    self.send_event(Event::terminated()).await?;
                } else if !self.control.finished.load(Ordering::Relaxed) {
                    self.control
                        .terminate_requested
                        .store(true, Ordering::Relaxed);
                    // wake a script that's parked
                    if self.paused {
                        self.paused = false;
                        if let Some(ref tx) = self.cmd_sender {
                            let _ = tx.send(ExecutionCommand::Continue);
                        }
                    }
                    let control = Arc::clone(&self.control);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        if !control.finished.load(Ordering::Relaxed) {
                            std::process::exit(0);
                        }
                    });
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "continue" => {
                self.paused = false;
                if let Some(ref tx) = self.cmd_sender {
                    let _ = tx.send(ExecutionCommand::Continue);
                }
                let body = json!({ "allThreadsContinued": true });
                self.send_response(Response::success(request_seq, &command, Some(body)))
                    .await?;
            }

            "next" => {
                self.paused = false;
                if let Some(ref tx) = self.cmd_sender {
                    let _ = tx.send(ExecutionCommand::StepOver);
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "stepIn" => {
                self.paused = false;
                if let Some(ref tx) = self.cmd_sender {
                    let _ = tx.send(ExecutionCommand::StepIn);
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            "stepOut" => {
                self.paused = false;
                if let Some(ref tx) = self.cmd_sender {
                    let _ = tx.send(ExecutionCommand::StepOut);
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
            }

            // attached: a plain disconnect detaches and leaves the host running
            "disconnect" if self.attached => {
                let terminate = req
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("terminateDebuggee"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if terminate {
                    self.request_host_exit();
                }
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
                return Ok(true);
            }

            "disconnect" => {
                self.send_response(Response::success(request_seq, &command, None))
                    .await?;
                self.send_event(Event::terminated()).await?;
                return Ok(true); // Signal to terminate session loop
            }

            _ => {
                self.send_response(Response::error(
                    request_seq,
                    &command,
                    "Command not supported",
                ))
                .await?;
            }
        }

        Ok(false)
    }

    async fn send_response(&mut self, mut response: Response) -> Result<()> {
        response.seq = self.seq;
        self.seq += 1;
        let msg = serde_json::to_string(&response)?;
        trace("OUT", &msg);
        self.transport.write_msg(&msg).await?;
        Ok(())
    }

    async fn send_event(&mut self, mut event: Event) -> Result<()> {
        event.seq = self.seq;
        self.seq += 1;
        let msg = serde_json::to_string(&event)?;
        trace("OUT", &msg);
        self.transport.write_msg(&msg).await?;
        Ok(())
    }
}
