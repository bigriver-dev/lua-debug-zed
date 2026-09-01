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
        substitute_workspace_folder(&mut parsed_config, &root_path);
        let resolved_config = parsed_config.to_string();

        let lua_version = parsed_config
            .get("luaVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("5.3")
            .to_string();

        let request = self.dap_request_kind(adapter_name, parsed_config)?;

        // debug code
        // let (os, _arch) = zed::current_platform();
        // let binary_name = match os {
        //     zed::Os::Windows => "lua-dap-server.exe",
        //     _ => "lua-dap-server",
        // };

        // let command = user_provided_debug_adapter_path
        //     .or_else(|| worktree.which("lua-dap-server"))
        //     .unwrap_or_else(|| format!("{}/target/debug/{}", root_path, binary_name));

        let command = match user_provided_debug_adapter_path {
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
                       "Lua Debug Adapter only supports launching a script, not attaching to an existing process"
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
