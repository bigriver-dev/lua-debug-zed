/*
 * entry point to spawn internal thread inside target process to start the agent engine
 */

pub mod c_api;
pub mod hook;
pub mod ipc;
pub mod types;

/*
 * handle attach/detach events on Windows (DLL_PROCESS_ATTACH)
 * on dll call, disables thread library calls and spawns init_agent_thread
 */
pub fn dll_main(hinst_dll: HINSTANCE, reason: DWORD, _reserved: LPVOID) -> bool {}

/*
 * handle attach/detach on linux.maxos
 * executes on library load and spawns init_agent_thread
 */
#[ctor::ctor]
pub fn ctor_init() {}

/*
 * spawn OS thread to run agent_main
 * this is to prevent blocking the target app's main thread during injection
 */
pub fn init_agent_thread() {}

/*
 * main entry;
 * 1.) calls c_api::resolve_symbols
 * 2.) establish ipc connection
 * 3.) initalizes breakpoints
 */
pub fn agent_main() {}
