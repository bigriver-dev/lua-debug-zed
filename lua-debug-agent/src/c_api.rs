/*
 * FFI delclarations for interacting with the host process's lua c api
 *
 * lua_sethook
 * lua_getinfo
 * lua_getstack
 * lua_tolstring
 * lua_getlocal
 */

use crate::types::{HookCallback, LuaDebug, LuaState};

/*
 * scans loaded libraries in the current process to locate required
 * c api fpointers to populate global/thread function tables
 */
pub fn resolve_symbols() -> Result<(), String> {}

/*
 * set execution hook callback (debug.sethook())
 * https://ligurio.github.io/lua-c-manual-pages/lua_sethook.3.html
 */
pub fn lua_sethook(L: *mut LuaState, func: HookCallback, mask: i32, count: i32) -> i32 {}

/*
 * Gets information about a specific function or function invocation.
 * https://www.lua.org/manual/5.3/manual.html#lua_getinfo
 */
pub fn lua_getinfo(L: *mut LuaState, what: *const i8, ar: *mut LuaDebug) -> i32 {}

/*
 * Gets information about the interpreter runtime stack.
 * https://www.lua.org/manual/5.3/manual.html#lua_getstack
 */
pub fn lua_getstack(L: *mut LuaState, level: i32, ar: *mut LuaDebug) -> i32 {}

/*
 * Converts the Lua value at the given index to a C string.
 * https://www.lua.org/manual/5.3/manual.html#lua_tolstring
 */
pub fn lua_tolstring(L: *mut LuaState, idx: i32, len: *mut usize) -> *const i8 {}

/*
 * Gets information about a local variable of a given activation record or a given function.
 * https://www.lua.org/manual/5.3/manual.html#lua_getlocal
 */
pub fn lua_getlocal(L: *mut LuaState, ar: *const LuaDebug, n: i32) -> *const i8 {}
