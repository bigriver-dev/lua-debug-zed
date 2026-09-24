/*
 * normalize file paths
 * track line breakpoints for active sessions
 */

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct BreakpointRegistry {
    // Maps normalized file paths to set of active line numbers
    file_breakpoints: HashMap<PathBuf, HashMap<usize, Option<String>>>,
    // Memoized `@source` chunk name -> normalized path.
    resolved: HashMap<String, PathBuf>,
    // count of armed breakpoints
    total: usize,
}

impl BreakpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /*
     * breakpoints for a given file and returns verification status
     */
    pub fn set_breakpoints(&mut self, path: PathBuf, breakpoints: Vec<(usize, Option<String>)>) {
        let normalized = normalize_path(&path);
        let map: HashMap<usize, Option<String>> = breakpoints.into_iter().collect();
        let added = map.len();
        let removed = self
            .file_breakpoints
            .insert(normalized, map)
            .map_or(0, |old| old.len());
        self.total = (self.total + added) - removed;
    }

    /*
     * clear breakpoints for a specific file
     */
    pub fn clear_breakpoints(&mut self, path: &Path) {
        let normalized = normalize_path(path);
        if let Some(old) = self.file_breakpoints.remove(&normalized) {
            self.total -= old.len();
        }
    }

    /*
     * True when nothing is armed
     */
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /*
     * Hot path lookup, called once per executed Lua line.
     */
    pub fn lookup(&mut self, raw_source: &str, line: usize) -> Option<Option<String>> {
        if self.total == 0 {
            return None;
        }

        if !self.resolved.contains_key(raw_source) {
            let cleaned = raw_source.strip_prefix('@').unwrap_or(raw_source);
            let normalized = normalize_path(Path::new(cleaned));
            self.resolved.insert(raw_source.to_string(), normalized);
        }
        let path = &self.resolved[raw_source];

        self.file_breakpoints
            .get(path)
            .and_then(|lines| lines.get(&line))
            .cloned()
    }
}

/*
 * similar to BreakpointRegistry, but bkps don't need any path
 */
#[derive(Debug, Default, Clone)]
pub struct FunctionBreakpointRegistry {
    functions: HashMap<String, Option<String>>,
}

impl FunctionBreakpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_breakpoints(&mut self, breakpoints: Vec<(String, Option<String>)>) {
        self.functions = breakpoints.into_iter().collect();
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub fn condition_for(&self, name: &str) -> Option<Option<String>> {
        self.functions.get(name).cloned()
    }
}

/*
 * normalizing paths garbanzo
 */
fn normalize_path(path: &Path) -> PathBuf {
    // Fast path: attempt canonicalization if possible, fallback to clean path representation
    let path_buf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Strip Windows verbatim prefix (`\?\`) if present to ensure reliable matching across DAP clients
    // thanks claude for this; I hate pattern matching/regex
    #[cfg(windows)]
    {
        let path_str = path_buf.to_string_lossy();
        if path_str.starts_with(r"\?\") {
            return PathBuf::from(&path_str[4..]);
        }
    }

    path_buf
}
