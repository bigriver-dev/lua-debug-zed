# lua-debug-zed

This is a lua debugger built in rust, where the lua vm is provided by the mlua package.

Features:

- load external dynamic libraries (.dll/.so)
- version support (`5.1-5.5+luajit`)
- breakpoints
- function breakpoints (this is implemented, but zed currently doesn't support this.)
- conditional breakpoints
- exception / exception breakpoints
- upvalues
- variables
- set variables while in breakpoint
- step over, step in, step out
- watches
- evaluate expressions
- package path/cpath support
- debug programs that embed lua (LÖVE, etc.)
- logpoints
- hit-count conditions
- pause
- stopOnEntry

# Debugger installation (current method)

1.) `rustup target add wasm32-wasip2` (This is required to install a local zed extension)

2.) Download `lua-debugger-extension-source.zip` & the binary `lua-dap-server-xxx.zip` for your OS.

3.) Unzip Extension. In the extracted folder, run `cargo build --target wasm32-wasip2 --release`

> you will need to download [wasi-sdk](https://github.com/WebAssembly/wasi-sdk) and place it on `PATH`

4.) Open up zed `extensions`. top-right, select `install dev extension`. point it to the unzipped folder.

5.) Unzip `lua-dap-server-xxx.zip`. In the Zed `settings.json` point to the directory like as follows:

```json
{
  "dap": {
    "lua": {
      "binary": "D:/GitHub/lua-debug-zed/dap-binary",
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
      "luaVersion": "5.3",
      // you can also target the executable directly
      //"debugServerPath": "${workspaceFolder}/target/debug/lua-dap-server.exe",
      // you can put path/cpath in the debugger global as well
      "packageCpath": ["../luarocks/lib/lua/5.3/?.dll"],
      "packagePath": ["../luarocks/share/lua/5.3/?.lua"],
    }
]
```

or put your `path` and `cpath` at the debugger level:
```json
{
  "lsp": {
    "lua-debugger": {
      "settings": {
        "packagePath":  ["${workspaceFolder}/src/?.lua"],
        "packageCpath": ["C:/libs/?.dll"]
      }
    }
  }
}
```

> Zed will complain that it can't find `"lua-debugger"` because Zed only knows language servers, but it will work.

# Debugging programs that embed lua (LÖVE)

In your starting code file (`conf.lua` in LÖVE), put the following:
```lua
if os.getenv("LUADEBUG_PORT") then require("luadebug").start() end
```

> The server puts the debugger module on the program’s LUA_CPATH, so the game needs no path setup

Within your `/.zed/debug.json`, add a debugging profile:

```json
  { 
    "adapter": "lua",
    "label": "Debug LÖVE game",
    "request": "launch",
    //runs `lovec` on Windows, `love` on linux/macos on PATH
    "preset": "love", 
    // only needed if love isn't in PATH
    // "runtimeExecutable": "C:/path/to/lovec.exe"
  }
```

# Zed Extension Installation for Developing

1.) Clone repo

2.) `rustup target add wasm32-wasip2`

> Build WASM extension
`cargo build --target wasm32-wasip2 -p zed-lua-debug-extension --release`

3.) open up zed `extensions`. Top-right, select `install dev extension`. Point it to the `./zed-extension` directory of this project

> If on windows, run the `.ps1` script to build the necessary `.dll` files to link against

4.) build dap server `cargo build -p lua-dap-server --bin lua-dap-server --no-default-features --features lua54`

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

Where `debugServerPath` points to binary generated from lua-dap-server. This variable overrides `luaVersion` and anything in `settings.json`.

# Debugging DAP

> Run Zed Debugger (lldb)

> type in terminal:
`Content-Length: 71\r\n\r\n{"seq":1,"type":"request","command":"initialize","arguments":{}}`
