/*
 * `launch` with `runtimeExecutable`
 * start a host (i.e. love) program that loads lua-debug-module
 * connect to the DAP server and relay between that and Zed.
 */

use crate::dap::protocol::LaunchRequestArguments;
use crate::dap::transport::DapTransport;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

// luadebug.start()
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const EXIT_GRACE: Duration = Duration::from_secs(3);

const START_HINT: &str = "The program has to load the debugger, e.g. in a LÖVE game's \
    conf.lua: if os.getenv(\"LUADEBUG_PORT\") then require(\"luadebug\").start() end";

#[cfg(windows)]
const MODULE_EXT: &str = "dll";
#[cfg(not(windows))]
const MODULE_EXT: &str = "so";

type GameTransport = DapTransport<OwnedReadHalf, OwnedWriteHalf>;
type Console = Vec<(&'static str, String)>;

pub struct LaunchedHost {
    child: Child,
    game: GameTransport,
    output: UnboundedReceiver<(&'static str, String)>,
}

/*
 * Starts the host and connects to its luadebug server. `raw_args` (the debug.json
 * entry) is passed on as the attach arguments, for options like sourceRoot.
 */
pub async fn launch(
    args: &LaunchRequestArguments,
    raw_args: &Value,
    console: &mut Console,
) -> Result<LaunchedHost> {
    let name = args
        .runtime_executable
        .as_deref()
        .unwrap_or_default()
        .trim();
    let cwd = args.cwd.as_deref().map(Path::new);
    let exe = resolve_executable(name, cwd)?;
    let port = free_port()?;

    let mut cmd = Command::new(&exe);
    cmd.args(&args.runtime_args)
        .env("LUADEBUG_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in &args.env {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
    if let Some(cpath) = module_cpath(args, console)? {
        cmd.env("LUA_CPATH", cpath);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = cmd
        .spawn()
        .with_context(|| format!("can't start {}", exe.display()))?;
    let (out_tx, mut output) = unbounded_channel();
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, "stdout", out_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, "stderr", out_tx);
    }

    match connect(&mut child, port, raw_args).await {
        Ok(game) => Ok(LaunchedHost {
            child,
            game,
            output,
        }),
        Err(err) => {
            let _ = child.kill().await;
            drain_output(&mut output, console).await;
            Err(err.context(format!("{} didn't start a debugger", exe.display())))
        }
    }
}

/*
 * Waits for the host's luadebug server and attaches to it
 */
async fn connect(child: &mut Child, port: u16, attach_args: &Value) -> Result<GameTransport> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let stream = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            break stream;
        }
        if let Some(status) = child.try_wait()? {
            bail!("it exited ({status}) before luadebug was listening. {START_HINT}");
        }
        if Instant::now() > deadline {
            bail!(
                "luadebug wasn't listening on port {port} after {}s. {START_HINT}",
                CONNECT_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    stream.set_nodelay(true)?;
    let (reader, writer) = stream.into_split();
    let mut game = DapTransport::new(reader, writer);

    let initialize = json!({
        "seq": 1, "type": "request", "command": "initialize",
        "arguments": { "adapterID": "lua", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path" },
    });
    let attach =
        json!({ "seq": 2, "type": "request", "command": "attach", "arguments": attach_args });
    game.write_msg(&initialize.to_string()).await?;
    game.write_msg(&attach.to_string()).await?;

    let handshake = async {
        loop {
            let raw = game
                .read_msg()
                .await?
                .ok_or_else(|| anyhow!("the program closed the debugger connection"))?;
            let msg: Value = serde_json::from_str(&raw)?;
            if msg["type"] == "response" && msg["command"] == "attach" && msg["success"] != true {
                bail!(
                    "attach failed: {}",
                    msg["message"].as_str().unwrap_or("unknown error")
                );
            }
            if msg["event"] == "initialized" {
                return Ok(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), handshake)
        .await
        .map_err(|_| anyhow!("luadebug didn't answer the attach request"))??;
    Ok(game)
}

/*
 * Relays between Zed and the host until Zed disconnects
 * then makes sure a host that was asked to stop does
 */
pub async fn relay<R, W>(
    zed: &mut DapTransport<R, W>,
    host: LaunchedHost,
    mut seq: i64,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let LaunchedHost {
        mut child,
        game,
        mut output,
    } = host;
    let mut game = Some(game); // None once the host has closed the connection
    let mut exited = false;
    let mut terminate_host = true;
    let mut disconnect_seq: Option<i64> = None;

    let mut send = async |zed: &mut DapTransport<R, W>, mut msg: Value| -> Result<()> {
        msg["seq"] = json!(seq);
        seq += 1;
        zed.write_msg(&msg.to_string()).await
    };

    loop {
        tokio::select! {
            msg = zed.read_msg() => {
                let Some(raw) = msg? else { break };
                let Ok(req) = serde_json::from_str::<Value>(&raw) else { continue };
                let command = req["command"].as_str().unwrap_or_default().to_string();
                let request_seq = req["seq"].as_i64().unwrap_or_default();
                if command == "disconnect" {
                    terminate_host = req["arguments"]["terminateDebuggee"].as_bool().unwrap_or(true);
                }
                if let Some(g) = game.as_mut() {
                    if g.write_msg(&raw).await.is_ok() {
                        if command == "disconnect" {
                            disconnect_seq = Some(request_seq);
                        }
                        continue;
                    }
                    game = None;
                }
                // the host is gone
                let ok = matches!(command.as_str(), "disconnect" | "terminate");
                let mut response = json!({
                    "type": "response", "request_seq": request_seq, "command": command, "success": ok,
                });
                if !ok {
                    response["message"] = json!("The program has exited");
                }
                send(zed, response).await?;
                if command == "disconnect" {
                    break;
                }
            }
            msg = recv_game(&mut game) => match msg {
                Some(raw) => {
                    let Ok(msg) = serde_json::from_str::<Value>(&raw) else { continue };
                    let answers_disconnect = msg["type"] == "response"
                        && disconnect_seq.is_some()
                        && msg["request_seq"].as_i64() == disconnect_seq;
                    send(zed, msg).await?;
                    if answers_disconnect {
                        break;
                    }
                }
                None => {
                    game = None;
                    if let Some(request_seq) = disconnect_seq {
                        let response = json!({
                            "type": "response", "request_seq": request_seq, "command": "disconnect", "success": true,
                        });
                        send(zed, response).await?;
                        break;
                    }
                }
            },
            Some((category, text)) = output.recv() => {
                send(zed, output_event(category, &text)).await?;
            }
            status = child.wait(), if !exited => {
                exited = true;
                // the last of its output
                let mut tail = Vec::new();
                drain_output(&mut output, &mut tail).await;
                for (category, text) in tail {
                    send(zed, output_event(category, &text)).await?;
                }
                let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
                send(zed, json!({ "type": "event", "event": "exited", "body": { "exitCode": code } })).await?;
                send(zed, json!({ "type": "event", "event": "terminated" })).await?;
            }
        }
    }

    // a stopped host quits on its own
    if !exited
        && terminate_host
        && tokio::time::timeout(EXIT_GRACE, child.wait())
            .await
            .is_err()
    {
        let _ = child.kill().await;
    }
    Ok(())
}

async fn recv_game(game: &mut Option<GameTransport>) -> Option<String> {
    match game {
        Some(g) => g.read_msg().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

fn output_event(category: &str, text: &str) -> Value {
    json!({ "type": "event", "event": "output", "body": { "category": category, "output": text } })
}

// forwards a pipe as text one line at a time
fn pump(
    mut pipe: impl AsyncRead + Unpin + Send + 'static,
    category: &'static str,
    tx: UnboundedSender<(&'static str, String)>,
) {
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let read = if pending.is_empty() {
                Some(pipe.read(&mut chunk).await)
            } else {
                tokio::time::timeout(Duration::from_millis(50), pipe.read(&mut chunk))
                    .await
                    .ok()
            };
            let cut = match read {
                Some(Ok(0)) | Some(Err(_)) => break,
                Some(Ok(n)) => {
                    pending.extend_from_slice(&chunk[..n]);
                    match pending.iter().rposition(|&b| b == b'\n') {
                        Some(i) => i + 1,
                        None => continue,
                    }
                }
                None => pending.len(), // quiet: send the partial line
            };
            let text = String::from_utf8_lossy(&pending[..cut]).into_owned();
            pending.drain(..cut);
            if tx.send((category, text)).is_err() {
                return;
            }
        }
        if !pending.is_empty() {
            let _ = tx.send((category, String::from_utf8_lossy(&pending).into_owned()));
        }
    });
}

// whatever the host printed until its pipes close
async fn drain_output(
    output: &mut UnboundedReceiver<(&'static str, String)>,
    console: &mut Console,
) {
    while let Ok(Some(line)) = tokio::time::timeout(Duration::from_millis(300), output.recv()).await
    {
        console.push(line);
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/*
 * LUA_CPATH for the host
 */
fn module_cpath(args: &LaunchRequestArguments, console: &mut Console) -> Result<Option<String>> {
    let versioned = format!("luadebug-{}.{MODULE_EXT}", lua_feature());
    let plain = format!("luadebug.{MODULE_EXT}");
    let find = |dir: &Path| {
        [&versioned, &plain]
            .into_iter()
            .find(|name| dir.join(name).is_file())
            .cloned()
    };

    let configured = args.module_path.as_deref().filter(|p| !p.trim().is_empty());
    let (dir, file) = match configured {
        Some(p) => {
            let path = PathBuf::from(p);
            if path.is_dir() {
                match find(&path) {
                    Some(file) => (path, file),
                    None => bail!("modulePath '{p}' has no {versioned} or {plain}"),
                }
            } else if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let suffix = format!(".{MODULE_EXT}");
                let named_right =
                    name == plain || (name.starts_with("luadebug-") && name.ends_with(&suffix));
                if !named_right {
                    bail!(
                        "modulePath '{p}' must be named {plain} or luadebug-<version>{suffix}: \
                         require(\"luadebug\") looks for those names"
                    );
                }
                let file = name.to_string();
                (
                    path.parent().map(Path::to_path_buf).unwrap_or_default(),
                    file,
                )
            } else {
                bail!("modulePath '{p}' doesn't exist");
            }
        }
        None => {
            let dir = std::env::current_exe()?
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            match find(&dir) {
                Some(file) => (dir, file),
                None => {
                    console.push((
                        "console",
                        format!(
                            "lua-dap-server: no {versioned} or {plain} in {}; the program has to \
                             find it on its own package.cpath. Set \"modulePath\" to the folder \
                             holding it.\n",
                            dir.display()
                        ),
                    ));
                    return Ok(None);
                }
            }
        }
    };

    // luadebug-lua54.dll -> ?-lua54.dll
    let entry = dir
        .join(file.replacen("luadebug", "?", 1))
        .to_string_lossy()
        .into_owned();
    // keep a LUA_CPATH the user set (in `env` or inherited); `;;` keeps Lua's defaults
    let existing = match args.env.get("LUA_CPATH") {
        Some(value) => value.clone(),
        None => std::env::var("LUA_CPATH").ok(),
    };
    Ok(Some(match existing.filter(|s| !s.is_empty()) {
        Some(rest) => format!("{entry};{rest}"),
        None => format!("{entry};;"),
    }))
}

// the Lua this server was built for: luaVersion picks the server, the server the module
fn lua_feature() -> &'static str {
    if cfg!(feature = "luajit") {
        "luajit"
    } else if cfg!(feature = "lua51") {
        "lua51"
    } else if cfg!(feature = "lua52") {
        "lua52"
    } else if cfg!(feature = "lua54") {
        "lua54"
    } else if cfg!(feature = "lua55") {
        "lua55"
    } else {
        "lua53"
    }
}

/*
 * `lovec.exe`, `love` or a PATH
 */
fn resolve_executable(name: &str, cwd: Option<&Path>) -> Result<PathBuf> {
    if name.is_empty() {
        bail!("runtimeExecutable is empty");
    }
    let given = Path::new(name);
    if given.is_absolute() || given.components().count() > 1 {
        let path = match cwd {
            Some(dir) if given.is_relative() => dir.join(given),
            _ => given.to_path_buf(),
        };
        return candidates(&path)
            .into_iter()
            .find(|p| is_executable(p))
            .ok_or_else(|| anyhow!("runtimeExecutable '{}' doesn't exist", path.display()));
    }

    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(registry_path_dirs());
    dirs.iter()
        .flat_map(|dir| candidates(&dir.join(name)))
        .find(|p| is_executable(p))
        .ok_or_else(|| {
            anyhow!(
                "runtimeExecutable '{name}' wasn't found on PATH. Use its full path, or add its \
                 folder to your PATH"
            )
        })
}

// Windows runs as love.exe
fn candidates(path: &Path) -> Vec<PathBuf> {
    if !cfg!(windows) || path.extension().is_some() {
        return vec![path.to_path_buf()];
    }
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|ext| {
            let mut p = path.as_os_str().to_owned();
            p.push(ext);
            PathBuf::from(p)
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    path.is_file()
}

#[cfg(windows)]
fn registry_path_dirs() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    // Windows checks PATH for system then user
    let sources = [
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
        (HKEY_CURRENT_USER, "Environment"),
    ];
    sources
        .into_iter()
        .filter_map(|(hive, key)| {
            RegKey::predef(hive)
                .open_subkey(key)
                .ok()?
                .get_value::<String, _>("Path")
                .ok()
        })
        .flat_map(|value| {
            expand_env_vars(&value)
                .split(';')
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(not(windows))]
fn registry_path_dirs() -> Vec<PathBuf> {
    Vec::new()
}

// registry PATH entries are REG_EXPAND_SZ: %SystemRoot%\system32
#[cfg(windows)]
fn expand_env_vars(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bare_names_on_path() {
        let exe = if cfg!(windows) { "cmd.exe" } else { "sh" };
        assert!(resolve_executable(exe, None).unwrap().is_absolute());
        if cfg!(windows) {
            // no extension: PATHEXT supplies it
            assert!(resolve_executable("cmd", None).is_ok());
        }
        assert!(resolve_executable("surely-not-a-real-program-xyz", None).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn reads_registry_path() {
        let dirs = registry_path_dirs();
        assert!(!dirs.is_empty());
        assert!(
            dirs.iter()
                .all(|d| !d.to_string_lossy().contains("%SystemRoot%"))
        );
    }
}
