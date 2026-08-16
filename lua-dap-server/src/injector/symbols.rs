/*
 * performs memory pattern scanning to locate non-exported Lua c api functions
 * if the target executable links lua statically
 *
 * lua_sethook
 * lua_getstack
 * lua_getinfo
 */
