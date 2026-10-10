/*
 * normalize file paths
 * track line breakpoints for active sessions
 */

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitCondition {
    op: HitOp,
    n: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitOp {
    Gt,
    Ge,
    Eq,
    Ne,
    Lt,
    Le,
    Every,
}

impl HitCondition {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (op, rest) = [
            (">=", HitOp::Ge),
            ("<=", HitOp::Le),
            ("==", HitOp::Eq),
            ("!=", HitOp::Ne),
            (">", HitOp::Gt),
            ("<", HitOp::Lt),
            ("%", HitOp::Every),
        ]
        .into_iter()
        .find_map(|(sym, op)| text.strip_prefix(sym).map(|rest| (op, rest)))
        .unwrap_or((HitOp::Eq, text));

        let n: u64 = rest.trim().parse().ok()?;
        if op == HitOp::Every && n == 0 {
            return None;
        }
        Some(Self { op, n })
    }

    /*
     * `hits` is the 1-based count of times this breakpoint's condition has been satisfied
     */
    pub fn matches(&self, hits: u64) -> bool {
        match self.op {
            HitOp::Gt => hits > self.n,
            HitOp::Ge => hits >= self.n,
            HitOp::Eq => hits == self.n,
            HitOp::Ne => hits != self.n,
            HitOp::Lt => hits < self.n,
            HitOp::Le => hits <= self.n,
            HitOp::Every => hits % self.n == 0,
        }
    }
}

/*
 * One line breakpoint as requested by the client
 */
#[derive(Debug, Clone, Default)]
pub struct SourceBp {
    pub line: usize,
    pub condition: Option<String>,
    pub hit_condition: Option<HitCondition>,
    pub log_message: Option<String>,
}

/*
 * What the hook needs to know about a breakpoint on the line it just reached
 */
#[derive(Debug, Clone)]
pub struct BreakpointSpec {
    pub condition: Option<String>,
    pub hit_condition: Option<HitCondition>,
    pub log_message: Option<String>,
}

#[derive(Debug, Clone)]
struct ArmedBp {
    spec: BreakpointSpec,
    // times the condition has been satisfied so far
    hits: u64,
}

#[derive(Debug, Default, Clone)]
pub struct BreakpointRegistry {
    // Maps normalized file paths to the armed breakpoints on each line
    file_breakpoints: HashMap<PathBuf, HashMap<usize, ArmedBp>>,
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
    pub fn set_breakpoints(&mut self, path: PathBuf, breakpoints: Vec<SourceBp>) {
        let normalized = normalize_path(&path);
        let old = self
            .file_breakpoints
            .remove(&normalized)
            .unwrap_or_default();
        self.total -= old.len();

        let map: HashMap<usize, ArmedBp> = breakpoints
            .into_iter()
            .map(|bp| {
                let hits = old
                    .get(&bp.line)
                    .filter(|prev| prev.spec.hit_condition == bp.hit_condition)
                    .map_or(0, |prev| prev.hits);
                let armed = ArmedBp {
                    spec: BreakpointSpec {
                        condition: bp.condition,
                        hit_condition: bp.hit_condition,
                        log_message: bp.log_message,
                    },
                    hits,
                };
                (bp.line, armed)
            })
            .collect();

        self.total += map.len();
        self.file_breakpoints.insert(normalized, map);
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
    pub fn lookup(&mut self, raw_source: &str, line: usize) -> Option<BreakpointSpec> {
        self.armed_mut(raw_source, line).map(|bp| bp.spec.clone())
    }

    /*
     * Counts one satisfied hit of the breakpoint at this location and returns the new total.
     * Only called for breakpoints that carry a hit condition.
     */
    pub fn record_hit(&mut self, raw_source: &str, line: usize) -> u64 {
        match self.armed_mut(raw_source, line) {
            Some(bp) => {
                bp.hits += 1;
                bp.hits
            }
            None => 0,
        }
    }

    fn armed_mut(&mut self, raw_source: &str, line: usize) -> Option<&mut ArmedBp> {
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
            .get_mut(path)
            .and_then(|lines| lines.get_mut(&line))
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
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    // Fast path: attempt canonicalization if possible, fallback to clean path representation
    let path_buf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Strip Windows verbatim prefix (`\\?\`) if present to ensure reliable matching across DAP clients
    // thanks claude for this; I hate pattern matching/regex
    #[cfg(windows)]
    {
        let path_str = path_buf.to_string_lossy();
        if let Some(stripped) = path_str.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }

    path_buf
}
