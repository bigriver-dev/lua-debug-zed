use zed_extension_api::serde_json as json;
use zed_extension_api::{self as zed};

struct LuaDebugExtension;

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
        Ok(zed::DebugAdapterBinary {
            command: None,
            arguments: vec![],
            cwd: Some(worktree.root_path()),
            envs: vec![],
            request_args: zed::StartDebuggingRequestArguments {
                configuration: config.config,
                request: zed::StartDebuggingRequestArgumentsRequest::Attach,
            },
            connection: config
                .tcp_connection
                .map(|x| zed::resolve_tcp_template(x).map(Some).unwrap_or_default())
                .unwrap_or_default()
                .or(Some(zed::TcpArguments {
                    host: 0x7f000001,
                    port: 8173,
                    timeout: None,
                })),
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        _config: json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest, String> {
        Ok(zed::StartDebuggingRequestArgumentsRequest::Attach)
    }
}

zed::register_extension!(LuaDebugExtension);
