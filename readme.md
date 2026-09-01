# lua-debug-zed

This is lua debugger built in rust, where the lua vm is provided by the mlua package.

Features:

- load external dynamic libraries (.dll/.so)
- version support (`5.1-5.5+luajit`)
- breakpoints
- function breakpoints (+TODO)
- conditional breakpoints (+TODO)
- variable
- step over, step in, step out
- watches
- evaluate expressions
- exception

# Debugger installation (current method)

1.) `rustup target add wasm32-wasip2` (This is required to install a local zed extension)

2.) Download `lua-debugger-extension-source.zip`; If limited internet access, you can download the binary too `lua-dap-xxx.zip`

3.) unzip extension. open up zed `extensions`. top-right, select `install dev extension`. point it to the unzipped folder.

> This will attempt to download the correct binaries for your machine.

# Building 

> Add WASM target
`rustup target add wasm32-wasip2`

> Build WASM extension
`cargo build --target wasm32-wasip2 -p zed-lua-debug-extension --release`

> Build debug server
`cargo build -p lua-dap-server --release`

# Zed Installation for Developing

1.) Clone repo

2.) `rustup target add wasm32-wasip2`

3.) open up zed `extensions`. Top-right, select `install dev extension`. Point it to the `./zed-extension` directory of this project

4.) build dap server `cargo build --bin lua-dap-server`

5.) create `debug.json` file in the directory

```json
[
    {
      "adapter": "lua",
      "label": "Debug a Lua script",
      "request": "launch",
      "program": "${workspaceFolder}/main.lua",
      "stopOnEntry": false,
      "preloadPaths": ["${workspaceFolder}/debug"],
      "luaVersion": "5.3"
    }
]
```

# Debugging DAP

> Run Zed Debugger (lldb)

> type in terminal:
`Content-Length: 71\r\n\r\n{"seq":1,"type":"request","command":"initialize","arguments":{}}`
