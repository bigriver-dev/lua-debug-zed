/*
 * Inspects scope variables (locals, upvalues, globals) and evaluates arbitrary Lua expressions
 */

use mlua::{Function, Lua, Result, Table, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DapVariable {
    pub name: String,
    pub value: String,
    pub var_type: String,
    pub variables_reference: usize, // Non-zero if structured object (e.g., table)
}

/*
 * for storing lua table items
 */
pub struct TableRegistry {
    tables: HashMap<usize, Table>,
    next_id: usize,
}

impl TableRegistry {
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
            next_id: 10_000, //todo make configurable?
        }
    }

    fn register(&mut self, table: Table) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.tables.insert(id, table);
        id
    }

    fn get(&self, id: usize) -> Option<Table> {
        self.tables.get(&id).cloned()
    }
}

pub struct Evaluator;

impl Evaluator {
    /*
     * Inspects local variables and upvalues for a target stack frame.
     */
    pub fn get_frame_variables(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        registry: &mut TableRegistry,
    ) -> Result<Vec<DapVariable>> {
        let locals = Self::collect_frame_locals(lua, frame_level, base_level)?;
        Ok(locals
            .into_iter()
            .map(|(name, value)| Self::format_dap_variable(name, &value, registry))
            .collect())
    }

    /*
     * inspects upvalues (variables captured from an enclosing scope) for a target stack frame.
     */
    pub fn get_frame_upvalues(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        registry: &mut TableRegistry,
    ) -> Result<Vec<DapVariable>> {
        let upvalues = Self::collect_frame_upvalues(lua, frame_level, base_level)?;
        Ok(upvalues
            .into_iter()
            .map(|(name, value)| Self::format_dap_variable(name, &value, registry))
            .collect())
    }

    /*
     * Lua global table (_G) as a flat list of variables.
     */
    pub fn get_globals(lua: &Lua, registry: &mut TableRegistry) -> Result<Vec<DapVariable>> {
        let mut vars = Vec::new();
        for pair in lua.globals().pairs::<Value, Value>() {
            let (key, value) = pair?;
            let name = match &key {
                Value::String(s) => s.to_string_lossy().to_owned(),
                other => format!("{:?}", other),
            };
            vars.push(Self::format_dap_variable(name, &value, registry));
        }
        vars.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(vars)
    }

    /*
     * Resolves a previously-registered table reference back into its kv pairs, for when the client expands a table in the UI.
     */
    pub fn get_table_contents(registry: &mut TableRegistry, table_ref: usize) -> Vec<DapVariable> {
        let Some(table) = registry.get(table_ref) else {
            return Vec::new();
        };

        let mut vars = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let Ok((key, value)) = pair else { continue };
            let name = match &key {
                Value::String(s) => s.to_string_lossy().to_owned(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                other => format!("{:?}", other),
            };
            vars.push(Self::format_dap_variable(name, &value, registry));
        }
        vars
    }

    /*
     * Executes string expressions within the lexical context of the selected stack frame.
     */
    pub fn evaluate_expression(
        lua: &mlua::Lua,
        frame_level: usize,
        base_level: usize,
        expr: &str,
        registry: &mut TableRegistry,
    ) -> Result<DapVariable> {
        let result = Self::eval_in_frame(lua, frame_level, base_level, expr)?;

        Ok(Self::format_dap_variable(
            expr.to_string(),
            &result,
            registry,
        ))
    }

    /*
     * set local variable
     */
    pub fn set_local(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        name: &str,
        value_expr: &str,
        registry: &mut TableRegistry,
    ) -> Result<DapVariable> {
        let new_value = Self::eval_in_frame(lua, frame_level, base_level, value_expr)?;
        let level = base_level + frame_level + 1;

        let debug_table: Table = lua.globals().get("debug")?;
        let getlocal: Function = debug_table.get("getlocal")?;
        let setlocal: Function = debug_table.get("setlocal")?;

        let mut i = 1;
        while let Ok((maybe_name, _)) = getlocal.call::<(Option<String>, Value)>((level, i)) {
            match maybe_name {
                Some(n) if n == name => {
                    setlocal.call::<Value>((level, i, new_value.clone()))?;
                    return Ok(Self::format_dap_variable(
                        name.to_string(),
                        &new_value,
                        registry,
                    ));
                }
                None => break,
                _ => {}
            }
            i += 1;
        }

        Err(mlua::Error::RuntimeError(format!(
            "local '{}' not found in this frame",
            name
        )))
    }

    /*
     * set upvalue
     */
    pub fn set_upvalue(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        name: &str,
        value_expr: &str,
        registry: &mut TableRegistry,
    ) -> Result<DapVariable> {
        let new_value = Self::eval_in_frame(lua, frame_level, base_level, value_expr)?;
        let level = base_level + frame_level;
        let Some(func) = lua.inspect_stack(level, |info| info.function()) else {
            return Err(mlua::Error::RuntimeError("no such frame".to_string()));
        };

        let debug_table: Table = lua.globals().get("debug")?;
        let getupvalue: Function = debug_table.get("getupvalue")?;
        let setupvalue: Function = debug_table.get("setupvalue")?;

        let mut i = 1;
        while let Ok((maybe_name, _)) =
            getupvalue.call::<(Option<String>, Value)>((func.clone(), i))
        {
            match maybe_name {
                Some(n) if n == name => {
                    setupvalue.call::<Value>((func.clone(), i, new_value.clone()))?;
                    return Ok(Self::format_dap_variable(
                        name.to_string(),
                        &new_value,
                        registry,
                    ));
                }
                None => break,
                _ => {}
            }
            i += 1;
        }

        Err(mlua::Error::RuntimeError(format!(
            "upvalue '{}' not found in this frame",
            name
        )))
    }

    /*
     * set a global; no frame needed
     */
    pub fn set_global(
        lua: &Lua,
        name: &str,
        value_expr: &str,
        registry: &mut TableRegistry,
    ) -> Result<DapVariable> {
        let chunk_code = format!("return ({})", value_expr);
        let new_value: Value = lua.load(&chunk_code).eval()?;
        lua.globals().set(name, new_value.clone())?;
        Ok(Self::format_dap_variable(
            name.to_string(),
            &new_value,
            registry,
        ))
    }

    /*
     * set key inside a previously-expanded table
     */
    pub fn set_table_value(
        lua: &Lua,
        registry: &mut TableRegistry,
        table_ref: usize,
        name: &str,
        value_expr: &str,
    ) -> Result<DapVariable> {
        let Some(table) = registry.get(table_ref) else {
            return Err(mlua::Error::RuntimeError(
                "table no longer available".to_string(),
            ));
        };

        let chunk_code = format!("return ({})", value_expr);
        let new_value: Value = lua.load(&chunk_code).eval()?;

        match name.parse::<i64>() {
            Ok(idx) => table.set(idx, new_value.clone())?,
            Err(_) => table.set(name, new_value.clone())?,
        }

        Ok(Self::format_dap_variable(
            name.to_string(),
            &new_value,
            registry,
        ))
    }

    /*
     * shared eval used by evaluate_expression
     */
    fn eval_in_frame(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        expr: &str,
    ) -> Result<Value> {
        let env = Self::build_frame_env(lua, frame_level, base_level)?;
        let chunk_code = format!("return ({})", expr);
        lua.load(&chunk_code).set_environment(env).eval()
    }

    /*
     * walks debug.getupvalue for a given frame
     */
    fn collect_frame_upvalues(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
    ) -> Result<Vec<(String, Value)>> {
        let level = base_level + frame_level;
        let Some(func) = lua.inspect_stack(level, |info| info.function()) else {
            return Ok(Vec::new());
        };

        let debug_table: Table = lua.globals().get("debug")?;
        let getupvalue: Function = debug_table.get("getupvalue")?;

        let mut upvalues = Vec::new();
        let mut i = 1;
        while let Ok((maybe_name, value)) =
            getupvalue.call::<(Option<String>, Value)>((func.clone(), i))
        {
            match maybe_name {
                // _ENV is useless for upvalues
                Some(name) if name != "_ENV" => upvalues.push((name, value)),
                Some(_) => {}
                None => break,
            }
            i += 1;
        }

        Ok(upvalues)
    }

    /*
     * walks debug.getlocal for a given frame
     */
    fn collect_frame_locals(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
    ) -> Result<Vec<(String, Value)>> {
        let debug_table: Table = lua.globals().get("debug")?;
        let getlocal: Function = debug_table.get("getlocal")?;

        let mut locals = Vec::new();
        let mut i = 1;
        while let Ok((maybe_name, value)) =
            getlocal.call::<(Option<String>, Value)>((base_level + frame_level + 1, i))
        {
            match maybe_name {
                Some(name) => {
                    if !name.starts_with('(') {
                        locals.push((name, value));
                    }
                }
                None => break,
            }
            i += 1;
        }

        Ok(locals)
    }

    /*
     * Sandboxed environment table for a given frame: fallback to globals
     */
    fn build_frame_env(lua: &Lua, frame_level: usize, base_level: usize) -> Result<Table> {
        let env = lua.create_table()?;
        let metatable = lua.create_table()?;
        metatable.set("__index", lua.globals())?;
        env.set_metatable(Some(metatable))?;

        for (name, value) in Self::collect_frame_locals(lua, frame_level, base_level)? {
            env.set(name, value)?;
        }

        Ok(env)
    }

    /*
     * Evaluate breakpoint returning lua truthy
     */
    pub fn evaluate_condition(
        lua: &Lua,
        frame_level: usize,
        base_level: usize,
        expr: &str,
    ) -> bool {
        let Ok(env) = Self::build_frame_env(lua, frame_level, base_level) else {
            return false;
        };
        let chunk_code = format!("return ({})", expr);
        match lua.load(&chunk_code).set_environment(env).eval::<Value>() {
            Ok(Value::Nil) | Ok(Value::Boolean(false)) => false,
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /*
     * Finds the inspect_stack level of the Lua frame that actually raised an error
     */
    pub fn find_error_frame_level(lua: &Lua) -> usize {
        for level in 0..64 {
            match lua.inspect_stack(level, |info| info.current_line().is_some()) {
                Some(true) => return level,
                Some(false) => continue,
                None => break,
            }
        }
        0
    }

    /*
     * error/assert with a string message, or a runtime error
     * error() can raise any value, so fall back to Lua's own tostring.
     */
    pub fn format_lua_error_value(lua: &Lua, val: &Value) -> String {
        if let Value::String(s) = val {
            return s.to_string_lossy();
        }
        if let Ok(tostring) = lua.globals().get::<Function>("tostring") {
            if let Ok(s) = tostring.call::<String>(val.clone()) {
                return s;
            }
        }
        format!("{:?}", val)
    }

    /*
     * peeks inside table contents to show preview of up to 20 chars
     */
    fn preview_table(t: &Table, count: &mut usize) -> String {
        const PREVIEW_CHARS: usize = 20;
        let mut preview = String::new();
        let mut truncated = false;
        for pair in t.pairs::<Value, Value>() {
            let Ok((key, val)) = pair else { continue };
            *count += 1;
            if preview.chars().count() >= PREVIEW_CHARS {
                truncated = true;
                continue;
            }
            if !preview.is_empty() {
                preview.push_str(", ");
            }
            preview.push_str(&Self::preview_entry(&key, &val));
        }
        if preview.chars().count() > PREVIEW_CHARS {
            truncated = true;
            preview = preview.chars().take(PREVIEW_CHARS).collect();
        }
        if truncated {
            preview.push_str("...");
        }
        preview
    }

    /*
     * format kv pair for preview_table
     */
    fn preview_entry(key: &Value, val: &Value) -> String {
        let val_str = match val {
            Value::Nil => "nil".to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => format!("\"{}\"", s.to_string_lossy()),
            Value::Table(_) => "table".to_string(),
            Value::Function(_) => "function".to_string(),
            _ => "?".to_string(),
        };
        match key {
            Value::Integer(_) => val_str,
            Value::String(s) => format!("{}={}", s.to_string_lossy(), val_str),
            other => format!("{:?}={}", other, val_str),
        }
    }

    /*
     * Formats an `mlua::Value` into a strongly typed `DapVariable` struct.
     */
    fn format_dap_variable(name: String, val: &Value, registry: &mut TableRegistry) -> DapVariable {
        let (val_str, var_type, reference) = match val {
            Value::Nil => ("nil".to_string(), "nil", 0),
            Value::Boolean(b) => (b.to_string(), "boolean", 0),
            Value::Integer(i) => (i.to_string(), "integer", 0),
            Value::Number(n) => (n.to_string(), "number", 0),
            Value::String(s) => (format!("\"{}\"", s.to_string_lossy()), "string", 0),
            Value::Table(t) => {
                let mut count = 0usize;
                let preview = Self::preview_table(t, &mut count);
                let reference = registry.register(t.clone());
                (
                    format!("table ({} items) {{{}}}", count, preview),
                    "table",
                    reference,
                )
            }
            Value::Function(f) => {
                let label = if f.info().what == "C" {
                    "C function"
                } else {
                    "function"
                };
                (label.to_string(), "function", 0)
            }
            Value::UserData(_) => ("userdata".to_string(), "userdata", 0),
            Value::LightUserData(_) => ("lightuserdata".to_string(), "lightuserdata", 0),
            Value::Thread(_) => ("thread".to_string(), "thread", 0),
            Value::Error(e) => (format!("error: {}", e), "error", 0),
            Value::Other(_) => ("other".to_string(), "other", 0),
        };

        DapVariable {
            name,
            value: val_str,
            var_type: var_type.to_string(),
            variables_reference: reference,
        }
    }
}
