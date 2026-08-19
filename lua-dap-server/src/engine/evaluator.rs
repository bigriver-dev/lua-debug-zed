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
        registry: &mut TableRegistry,
    ) -> Result<Vec<DapVariable>> {
        let mut vars = Vec::new();

        // Access Lua's standard `debug.getlocal` function via globals
        let debug_table: Table = lua.globals().get("debug")?;
        let getlocal: Function = debug_table.get("getlocal")?;

        let mut i = 1;
        // Lua stack frame levels in debug.getlocal are 1-based...
        while let Ok((maybe_name, value)) =
            getlocal.call::<(Option<String>, Value)>((frame_level + 1, i))
        {
            match maybe_name {
                Some(name) => {
                    if !name.starts_with('(') {
                        vars.push(Self::format_dap_variable(name, &value, registry));
                    }
                }
                None => break, // Reached end of local variables
            }
            i += 1;
        }

        Ok(vars)
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
        expr: &str,
        registry: &mut TableRegistry,
    ) -> Result<DapVariable> {
        let chunk_code = format!("return ({})", expr);

        // create a sandboxed evaluation environment table
        let env = lua.create_table()?;

        // fall back to globals via metatable __index
        let metatable = lua.create_table()?;
        metatable.set("__index", lua.globals())?;
        env.set_metatable(Some(metatable))?;

        // collect frame locals into the environment table via debug.getlocal
        let debug_table: Table = lua.globals().get("debug")?;
        let getlocal: Function = debug_table.get("getlocal")?;

        let mut i = 1;
        while let Ok((maybe_name, value)) =
            getlocal.call::<(Option<String>, Value)>((frame_level + 1, i))
        {
            match maybe_name {
                Some(name) => {
                    if !name.starts_with('(') {
                        env.set(name, value)?;
                    }
                }
                None => break,
            }
            i += 1;
        }

        // bind the environment to the compiled chunk and evaluate
        let result: Value = lua.load(&chunk_code).set_environment(env).eval()?;

        Ok(Self::format_dap_variable(
            expr.to_string(),
            &result,
            registry,
        ))
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
                let count = t.pairs::<Value, Value>().count();
                let reference = registry.register(t.clone());
                (format!("table ({} items)", count), "table", reference)
            }
            Value::Function(_) => ("function".to_string(), "function", 0),
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
