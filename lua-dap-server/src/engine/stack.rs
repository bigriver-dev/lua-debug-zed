/*
 * Inspects Lua stack frames using mlua::Debug context when paused at a breakpoint.
 */
use mlua::Lua;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackFrame {
    pub id: usize,
    pub name: String,
    pub source: PathBuf,
    pub line: usize,
}

impl StackFrame {
    /*
     * Iterates up the call stack via lua_debug levels to build a stack trace list
     */
    pub fn capture_stack_trace(lua: &Lua) -> Vec<StackFrame> {
        let mut frames = Vec::new();
        let mut level = 0;

        while let Some(frame) = lua.inspect_stack(level, |debug| {
            let name = debug
                .names()
                .name
                .map(|n| n.into_owned())
                .unwrap_or_else(|| {
                    if level == 0 {
                        "<current frame>".to_string()
                    } else {
                        format!("<anonymous@{}>", level)
                    }
                });

            let source_info = debug.source();
            let source_path = source_info
                .source
                .map(|s| {
                    // lua sometimes adds leading '@' character
                    let clean_path = s.strip_prefix('@').unwrap_or(&s);
                    PathBuf::from(clean_path)
                })
                .unwrap_or_else(|| PathBuf::from("[eval]"));

            let current_line = debug.current_line();

            StackFrame {
                id: level,
                name,
                source: source_path,
                line: current_line.filter(|&l| l > 0).unwrap_or(1),
            }
        }) {
            frames.push(frame);
            level += 1;
        }

        frames
    }
}
