//! Minimal argument parsing.
//!
//! Supports `--key value`, `--key=value`, and bare boolean flags named in
//! [`BOOL_FLAGS`]. Deliberately dependency-free: the collector's option surface
//! is small and fixed, and every dependency in this crate sits on the path
//! between untrusted bytes and disk.

use std::collections::{HashMap, HashSet};

/// Flags that are presence-only and never consume the following token.
pub const BOOL_FLAGS: &[&str] = &["dry-run", "initial", "no-sweep", "no-refetch", "help"];

/// Parsed command-line arguments.
pub struct Args {
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Args {
    /// Parse argument tokens (program name and subcommand already stripped).
    pub fn parse<I: IntoIterator<Item = String>>(tokens: I) -> Self {
        let toks: Vec<String> = tokens.into_iter().collect();
        let mut values = HashMap::new();
        let mut flags = HashSet::new();
        let mut i = 0;
        while i < toks.len() {
            let Some(rest) = toks[i].strip_prefix("--") else {
                i += 1;
                continue;
            };
            if let Some((k, v)) = rest.split_once('=') {
                if !v.is_empty() {
                    values.insert(k.to_string(), v.to_string());
                }
                i += 1;
            } else if BOOL_FLAGS.contains(&rest) {
                flags.insert(rest.to_string());
                i += 1;
            } else if i + 1 < toks.len() && !toks[i + 1].starts_with("--") {
                if !toks[i + 1].is_empty() {
                    values.insert(rest.to_string(), toks[i + 1].clone());
                }
                i += 2;
            } else {
                flags.insert(rest.to_string());
                i += 1;
            }
        }
        Self { values, flags }
    }

    /// An optional value flag.
    pub fn opt(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// A value flag with a default.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.opt(key).unwrap_or(default)
    }

    /// A numeric value flag with a default, erroring on a malformed value
    /// rather than silently falling back.
    pub fn num_or<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.opt(key) {
            Some(s) => s.parse().map_err(|_| format!("bad --{key} value {s:?}")),
            None => Ok(default),
        }
    }

    /// Whether a boolean flag was present.
    pub fn has(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Args {
        Args::parse(s.iter().map(|x| x.to_string()))
    }

    #[test]
    fn parses_values_and_bool_flags() {
        let a = args(&["--root", "/tmp/x", "--max-requests=50", "--dry-run"]);
        assert_eq!(a.opt("root"), Some("/tmp/x"));
        assert_eq!(a.num_or::<u32>("max-requests", 10).unwrap(), 50);
        assert!(a.has("dry-run"));
        assert_eq!(a.get_or("base", "https://1f916.ai"), "https://1f916.ai");
    }

    #[test]
    fn known_bool_flag_does_not_swallow_next_token() {
        let a = args(&["--dry-run", "--root", "/tmp/x"]);
        assert!(a.has("dry-run"));
        assert_eq!(a.opt("root"), Some("/tmp/x"));
    }

    #[test]
    fn malformed_number_is_an_error_not_a_silent_default() {
        // A typo'd budget must stop the run, not quietly restore the default and
        // hammer someone else's server with the wrong request count.
        let a = args(&["--max-requests", "fifty"]);
        assert!(a.num_or::<u32>("max-requests", 10).is_err());
    }

    #[test]
    fn empty_value_is_treated_as_absent() {
        assert!(args(&["--root="]).opt("root").is_none());
        assert!(args(&["--root", ""]).opt("root").is_none());
    }
}
