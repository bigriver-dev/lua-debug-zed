use zed_extension_api::serde_json as json;
use zed_extension_api::{self as zed};

struct LuaDebugExtension;

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
        let request = self.dap_request_kind(adapter_name, parsed_config)?;

        let (os, _arch) = zed::current_platform();
        let binary_name = match os {
            zed::Os::Windows => "lua-dap-server.exe",
            _ => "lua-dap-server",
        };

        let command = user_provided_debug_adapter_path
            .or_else(|| worktree.which("lua-dap-server"))
            .unwrap_or_else(|| format!("{}/target/debug/{}", root_path, binary_name));

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
