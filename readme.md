# lua-debug-zed

This is lua debugger built in rust, where the lua vm is provided by the mlua package.

Features:

- load external dynamic libraries (.dll/.so)
- version support (for now, you'll need to change the `mlua` crate version. `5.1-5.5+luajit` work)
- breakpoints
- function breakpoints
- conditional breakpoints
- variable
- step over, step in, step out
- watches
- evaluate expressions
- exception

# Building 

> Add WASM target
`rustup target add wasm32-wasip1`

> Build WASM extension
`cargo build --target wasm32-wasip1 -p zed-lua-debug-extension --release`

> Build debug server
`cargo build -p lua-dap-server --release`

# Zed Installation

1.) `rustup target add wasm32-wasip1`

2.) open up zed `extensions`

3.) top-right, select `install dev extension`

4.) point it to the `./zed-extension` directory of this project

5.) build dap server `cargo build --bin lua-dap-server`

6.) create `debug.json` file

```json
[
    {
      "adapter": "lua",
      "label": "Debug a Lua script",
      "request": "launch",
      "program": "${workspaceFolder}/main.lua",
      "stopOnEntry": false,
      "preloadPaths": ["${workspaceFolder}/debug"]
    }
]
```

# Debugging

> Run Zed Debugger (lldb)

> type in terminal:
`Content-Length: 71\r\n\r\n{"seq":1,"type":"request","command":"initialize","arguments":{}}`
