# lua-debug-zed

This is lua debugger built in rust, where the lua vm is provided by the mlua package.

Features:

- load external dynamic libraries (.dll/.so)
- version support (`5.1-5.5+luajit`)
- breakpoints
- function breakpoints (this is implemented, but zed currently doesn't support this.)
- conditional breakpoints
- variable
- step over, step in, step out
- watches
- evaluate expressions
- exception

# Debugger installation (current method)

1.) `rustup target add wasm32-wasip2` (This is required to install a local zed extension)

2.) Download `lua-debugger-extension-source.zip` & the binary `lua-dap-xxx.zip` for your OS.

3.) Unzip Extension. In the extracted folder, run `cargo build --target wasm32-wasip2 --release`

4.) Open up zed `extensions`. top-right, select `install dev extension`. point it to the unzipped folder.

5.) Unzip `lua-dap-xxx.zip`. In the Zed `settings.json` point to the directory like as follows:

```json
{
  "dap": {
    "lua": {
      "binary": "C:\\Users\\User\\Documents\\GitHub\\lua-debug-zed\\dap-binary",
    }
  }
}
```

6.) Then open the directory that you want to debug lua. In that directory create `.zed/debug.json`. Add as follows:
```json
[
    {
      "adapter": "lua",
      "label": "Debug a Lua script",
      "request": "launch",
      "program": "${workspaceFolder}/main.lua",
      "stopOnEntry": false,
      "preloadPaths": ["${workspaceFolder}/lib"],
      "luaVersion": "5.3"
    }
]
```


# Building 

> Add WASM target
`rustup target add wasm32-wasip2`

> Build WASM extension
`cargo build --target wasm32-wasip2 -p zed-lua-debug-extension --release`

> Build debug server
`cargo build -p lua-dap-server --release`

# Zed Extension Installation for Developing

1.) Clone repo

2.) `rustup target add wasm32-wasip2`

3.) open up zed `extensions`. Top-right, select `install dev extension`. Point it to the `./zed-extension` directory of this project

4.) build dap server `cargo build -p lua-dap-server --bin lua-dap-server`. Defaults to lua53, change `Cargo.toml` to build another version.

5.) create `debug.json` file in the directory like as follows:
```json
[
    {
      "adapter": "lua",
      "label": "Debug a Lua script",
      "request": "launch",
      "program": "${workspaceFolder}/main.lua",
      "stopOnEntry": false,
      "preloadPaths": ["${workspaceFolder}/lib"],
      "debugServerPath": "${workspaceFolder}/target/debug/lua-dap-server.exe",
      "luaVersion": "5.3"
    }
]
```

Where `debugServerPath` points to binary generated from lua-dap-server. This variable overwrites `luaVersion` and anything in `settings.json`.

# Debugging DAP

> Run Zed Debugger (lldb)

> type in terminal:
`Content-Length: 71\r\n\r\n{"seq":1,"type":"request","command":"initialize","arguments":{}}`
