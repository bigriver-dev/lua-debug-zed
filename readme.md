## Add WASM target
`rustup target add wasm32-wasip1`

## Build WASM extension
`cargo build --target wasm32-wasip1 -p zed-lua-debug-extension --release`

## Build debug server
`cargo build -p lua-dap-server --release`
