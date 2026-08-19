/*
 * normalize file paths
 * track line breakpoints for active sessions
 */

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct BreakpointRegistry {
    // Maps normalized file paths to set of active line numbers
    file_breakpoints: HashMap<PathBuf, HashSet<usize>>,
}

impl BreakpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /*
     * breakpoints for a given file and returns verification status
     */
    pub fn set_breakpoints(&mut self, path: PathBuf, lines: Vec<usize>) {
        let normalized = normalize_path(&path);
        let set: HashSet<usize> = lines.into_iter().collect();
        self.file_breakpoints.insert(normalized, set);
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
            .map_or(false, |lines| lines.contains(&line))
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
