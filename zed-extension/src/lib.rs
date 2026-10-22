use zed_extension_api::serde_json as json;
use zed_extension_api::{self as zed};

struct LuaDebugExtension;
const RELEASE_REPO: &str = "bigriver-dev/lua-debug-zed";

fn resolve_binary(lua_version: &str) -> Result<String, String> {
    let (os, arch) = zed::current_platform();
    let os_name = match os {
        zed::Os::Windows => "windows",
        zed::Os::Mac => "macos",
        zed::Os::Linux => "linux",
    };
    let arch_name = match arch {
        zed::Architecture::Aarch64 => "aarch64",
        zed::Architecture::X8664 => "x86_64",
        zed::Architecture::X86 => "x86",
    };
    let ext = if matches!(os, zed::Os::Windows) {
        ".exe"
    } else {
        ""
    };
    let feature = lua_feature_name(lua_version);

    let release = zed::latest_github_release(
        RELEASE_REPO,
        zed::GithubReleaseOptions {
            require_assets: true,
            pre_release: false,
        },
    )?;

    let bundle_asset_name = format!("lua-dap-server-{os_name}-{arch_name}.zip");
    let extract_dir = format!("lua-dap-server-{}-{os_name}-{arch_name}", release.version);
    let binary_path = format!("{extract_dir}/lua-dap-server-{feature}{ext}");

    if !std::path::Path::new(&binary_path).exists() {
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == bundle_asset_name)
            .ok_or_else(|| {
                format!(
                    "No release asset named '{bundle_asset_name}' found in lua-dap-server {}",
                    release.version
                )
            })?;

        zed::download_file(
            &asset.download_url,
            &extract_dir,
            zed::DownloadedFileType::Zip,
        )?;

        if !matches!(os, zed::Os::Windows) {
            for f in ["lua51", "lua52", "lua53", "lua54", "lua55", "luajit"] {
                let _ = zed::make_file_executable(&format!("{extract_dir}/lua-dap-server-{f}"));
            }
        }
    }

    Ok(binary_path)
}

fn lua_feature_name(version: &str) -> &str {
    match version {
        "5.1" => "lua51",
        "5.2" => "lua52",
        "5.4" => "lua54",
        "5.5" => "lua55",
        "luajit" => "luajit",
        _ => "lua53",
    }
}

fn substitute_workspace_folder(value: &mut json::Value, root_path: &str) {
    match value {
        json::Value::String(s) => {
            if s.contains("${workspaceFolder}") {
                *s = s.replace("${workspaceFolder}", root_path);
            }
        }
        json::Value::Array(items) => {
            for item in items {
                substitute_workspace_folder(item, root_path);
            }
        }
        json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                substitute_workspace_folder(v, root_path);
            }
        }
        _ => {}
    }
}

fn path_list(value: Option<&json::Value>) -> Vec<String> {
    match value {
        Some(json::Value::String(s)) => vec![s.clone()],
        Some(json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => vec![],
    }
}

fn merge_search_paths(config: &mut json::Value, key: &str, global: Option<&json::Value>) {
    let mut merged = path_list(config.get(key));
    merged.extend(path_list(global));
    if !merged.is_empty() {
        config[key] = json::Value::from(merged);
    }
}

/*
 * `"preset"`: defaults for a known host, filled in where the entry leaves them unset.
 * They're plain lua-debug-module options; the module itself knows no hosts.
 */
fn apply_preset(config: &mut json::Value) -> Result<(), String> {
    let Some(preset) = config.get("preset").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let is_unset = |config: &json::Value, key: &str| match config.get(key) {
        None | Some(json::Value::Null) => true,
        Some(json::Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    };
    match preset {
        // LÖVE 11+: LuaJIT, the game folder holds main.lua, and these two globals
        "love" => {
            // a game's own love.errorhandler, else LÖVE's default love.errhand
            let defaults = [
                ("luaVersion", json::json!("luajit")),
                ("sourceRoot", json::json!("${workspaceFolder}")),
                ("errorHandler", json::json!(["love.errorhandler", "love.errhand"])),
                ("onTerminate", json::json!("love.event.quit")),
            ];
            for (key, value) in defaults {
                if is_unset(config, key) {
                    config[key] = value;
                }
            }
            let launch = config.get("request").and_then(|v| v.as_str()) != Some("attach");
            if launch && is_unset(config, "program") && is_unset(config, "runtimeExecutable") {
                // lovec is the console build on Windows, so print() reaches Zed
                let (os, _) = zed::current_platform();
                let exe = if matches!(os, zed::Os::Windows) { "lovec" } else { "love" };
                config["runtimeExecutable"] = json::Value::from(exe);
                if config.get("runtimeArgs").is_none() {
                    config["runtimeArgs"] = json::json!([config["sourceRoot"].clone()]);
                }
            }
            Ok(())
        }
        other => Err(format!("Unknown \"preset\": \"{other}\" (known: \"love\")")),
    }
}

const ATTACH_HELP: &str = "\"request\": \"attach\" connects to a Lua program that loaded \
    lua-debug-module (require(\"luadebug\").start()). Add the port it listens on, e.g. \
    \"port\": 8172 (start()'s default). To attach to some other DAP server, use \
    \"tcp_connection\" instead.";

// Debug a DAP server that's already running (no adapter process to launch).
fn connect_to(
    connection: zed::TcpArguments,
    cwd: String,
    configuration: String,
    request: zed::StartDebuggingRequestArgumentsRequest,
) -> zed::DebugAdapterBinary {
    zed::DebugAdapterBinary {
        command: None,
        arguments: vec![],
        cwd: Some(cwd),
        envs: vec![],
        connection: Some(connection),
        request_args: zed::StartDebuggingRequestArguments {
            configuration,
            request,
        },
    }
}

impl zed::Extension for LuaDebugExtension {
    fn new() -> Self {
        Self
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary, String> {
        let root_path = worktree.root_path();
        let mut parsed_config: json::Value =
            json::from_str(&config.config).map_err(|e| e.to_string())?;

        let global_settings = zed::settings::LspSettings::for_worktree("lua-debugger", worktree)
            .ok()
            .and_then(|s| s.settings);
        for key in ["packagePath", "packageCpath"] {
            let global = global_settings.as_ref().and_then(|s| s.get(key));
            merge_search_paths(&mut parsed_config, key, global);
        }

        apply_preset(&mut parsed_config)?;

        // runtimeExecutable: the launched program loads luadebug.dll/.so
        // deafult dap.lua.binary folder
        let launches_host = parsed_config
            .get("runtimeExecutable")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        let has_module_path = parsed_config
            .get("modulePath")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        if launches_host && !has_module_path {
            if let Some(dir) = &user_provided_debug_adapter_path {
                parsed_config["modulePath"] = json::Value::String(dir.clone());
            }
        }

        substitute_workspace_folder(&mut parsed_config, &root_path);
        let resolved_config = parsed_config.to_string();

        let lua_version = parsed_config
            .get("luaVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("5.3")
            .to_string();

        let debug_server_path = parsed_config
            .get("debugServerPath")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // (require("luadebug").start()), listening on host:port
        let attach_host = parsed_config
            .get("host")
            .and_then(|v| v.as_str())
            .map(String::from);
        let attach_port = parsed_config.get("port").and_then(|v| v.as_u64());

        let request = self.dap_request_kind(adapter_name, parsed_config)?;

        // debug.json tcp_connection` support; connect to a DAP instance elsewhere
        if let Some(template) = config.tcp_connection.clone() {
            let connection = zed::resolve_tcp_template(template)?;
            return Ok(connect_to(connection, root_path, resolved_config, request));
        }

        if matches!(request, zed::StartDebuggingRequestArgumentsRequest::Attach) {
            let port = attach_port
                .and_then(|p| u16::try_from(p).ok())
                .ok_or_else(|| ATTACH_HELP.to_string())?;
            let host = match attach_host.as_deref() {
                None | Some("localhost") => std::net::Ipv4Addr::LOCALHOST,
                Some(h) => h.parse().map_err(|_| {
                    format!("\"host\": \"{h}\" must be an IPv4 address, e.g. 127.0.0.1")
                })?,
            };
            let connection = zed::resolve_tcp_template(zed::TcpArgumentsTemplate {
                host: Some(u32::from(host)),
                port: Some(port),
                timeout: None,
            })?;
            return Ok(connect_to(connection, root_path, resolved_config, request));
        }

        let command = if let Some(path) = debug_server_path {
            path
        } else {
            match user_provided_debug_adapter_path {
                Some(dir) => {
                    let (os, _arch) = zed::current_platform();
                    let ext = if matches!(os, zed::Os::Windows) {
                        ".exe"
                    } else {
                        ""
                    };
                    let feature = lua_feature_name(&lua_version);
                    format!("{dir}/lua-dap-server-{feature}{ext}")
                }
                None => resolve_binary(&lua_version)?,
            }
        };
        Ok(zed::DebugAdapterBinary {
            command: Some(command),
            arguments: vec![],
            cwd: Some(root_path),
            envs: vec![],
            connection: None,
            request_args: zed::StartDebuggingRequestArguments {
                configuration: resolved_config,
                request,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest, String> {
        match config.get("request").and_then(|v| v.as_str()) {
            Some("attach") => Ok(zed::StartDebuggingRequestArgumentsRequest::Attach),
            _ => Ok(zed::StartDebuggingRequestArgumentsRequest::Launch),
        }
    }

    fn dap_config_to_scenario(
        &mut self,
        config: zed::DebugConfig,
    ) -> Result<zed::DebugScenario, String> {
        let launch = match config.request {
            zed::DebugRequest::Launch(launch) => launch,
            zed::DebugRequest::Attach(_) => {
                return Err(
                    "Picking a process to attach to isn't supported. To debug a running Lua \
                    program, have it load lua-debug-module (require(\"luadebug\").start()) and \
                    add a .zed/debug.json entry with \"request\": \"attach\" and \"port\": 8172."
                        .to_string(),
                );
            }
        };

        let mut configuration = json::json!({
            "request": "launch",
            "program": launch.program,
        });
        if let Some(cwd) = launch.cwd {
            configuration["cwd"] = json::Value::String(cwd);
        }
        if !launch.args.is_empty() {
            configuration["args"] = json::Value::from(launch.args);
        }
        if let Some(stop_on_entry) = config.stop_on_entry {
            configuration["stopOnEntry"] = json::Value::Bool(stop_on_entry);
        }

        Ok(zed::DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: configuration.to_string(),
            tcp_connection: None,
        })
    }
}

zed::register_extension!(LuaDebugExtension);
