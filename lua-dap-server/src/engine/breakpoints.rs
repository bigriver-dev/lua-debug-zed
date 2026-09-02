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
        self.file_breakpoints.insert(normalized, map);
    }

    /*
     * clear breakpoints for a specific file
     */
    pub fn clear_breakpoints(&mut self, path: &Path) {
        let normalized = normalize_path(path);
        self.file_breakpoints.remove(&normalized);
    }

    /*
     * lookup called inside the line-hook hot path
     */
    pub fn is_breakpoint(&self, path: &Path, line: usize) -> bool {
        let normalized = normalize_path(path);
        self.file_breakpoints
            .get(&normalized)
            .is_some_and(|lines| lines.contains_key(&line))
    }

    /*
     * conditional breakpoint entry
     */
    pub fn condition_for(&self, path: &Path, line: usize) -> Option<String> {
        let normalized = normalize_path(path);
        self.file_breakpoints
            .get(&normalized)
            .and_then(|lines| lines.get(&line))
            .cloned()
            .flatten()
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

    // Strip Windows verbatim prefix (`\\?\`) if present to ensure reliable matching across DAP clients
    // thanks claude for this; I hate pattern matching/regex
    #[cfg(windows)]
    {
        let path_str = path_buf.to_string_lossy();
        if path_str.starts_with(r"\\?\") {
            return PathBuf::from(&path_str[4..]);
        }
    }

    path_buf
}
