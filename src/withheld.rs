//! The withholding register, and the check that keeps it honest.
//!
//! Payload bytes may be withheld (see `WITHHELD.md`); commitments never are. But
//! a withheld payload has a visible consequence: the next manifest is built over
//! the packets that remain, so the record leaves the *new* index and the diff
//! shows a removal. Older manifests and their anchors are untouched.
//!
//! That leaves two removals that look identical in a diff and are not remotely
//! alike:
//!
//! * a declared withholding — a deliberate, registered, reviewable decision, and
//! * evidence quietly disappearing.
//!
//! A prose promise in a README cannot tell them apart. This module can: every
//! record a diff removes must appear in `withheld.json`, naming the exact
//! version in which it was withheld. An undeclared removal fails the build,
//! before anything is signed.
//!
//! This is what makes "visibly withheld, never silently absent" a property of
//! the pipeline rather than an assurance about the maintainer.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "1f916-archive/withheld/v1";

/// One registered withholding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithheldRecord {
    pub shard_id: String,
    pub record_id: String,
    /// The manifest version in which this payload first left the index.
    pub withheld_at_version: u64,
    /// ISO-8601 date, for the human-readable register.
    pub date: String,
    /// Free-text category, e.g. `upstream-moderation-removal`.
    pub reason: String,
}

/// The register file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Register {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub withheld: Vec<WithheldRecord>,
}

impl Register {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            // No register means nothing has ever been withheld, which is a
            // perfectly ordinary state and not the same as a broken register.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    schema: SCHEMA.to_string(),
                    withheld: Vec::new(),
                })
            }
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let reg: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parsing {}: {e}", path.display()))?;
        if reg.schema != SCHEMA {
            return Err(format!(
                "{} has schema {:?}, expected {SCHEMA}",
                path.display(),
                reg.schema
            ));
        }
        Ok(reg)
    }
}

/// Minimal view of a `ManifestDiff`: only what the check needs.
#[derive(Debug, Deserialize)]
struct DiffView {
    child_version: u64,
    #[serde(default)]
    removed: Vec<RecordRefView>,
}

#[derive(Debug, Deserialize)]
struct RecordRefView {
    shard_id: String,
    record_id: String,
}

/// Check that every record removed by `diff` is declared in `register`, at this
/// diff's child version.
///
/// Returns the number of declared removals. Any undeclared removal, or one
/// declared against a different version, is an error naming the record.
pub fn check_removals(diff_path: &Path, register_path: &Path) -> Result<usize, String> {
    let bytes =
        std::fs::read(diff_path).map_err(|e| format!("reading {}: {e}", diff_path.display()))?;
    let diff: DiffView = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parsing {}: {e}", diff_path.display()))?;
    if diff.removed.is_empty() {
        return Ok(0);
    }
    let register = Register::load(register_path)?;

    // Pinning the version is what stops one old register entry from silently
    // covering a later, different removal of the same record.
    let declared: HashSet<(&str, &str)> = register
        .withheld
        .iter()
        .filter(|w| w.withheld_at_version == diff.child_version)
        .map(|w| (w.shard_id.as_str(), w.record_id.as_str()))
        .collect();

    let mut undeclared = Vec::new();
    for r in &diff.removed {
        if !declared.contains(&(r.shard_id.as_str(), r.record_id.as_str())) {
            undeclared.push(format!("{}/{}", r.shard_id, r.record_id));
        }
    }
    if !undeclared.is_empty() {
        undeclared.sort();
        return Err(format!(
            "v{} removes {} record(s) not declared in {} at that version:\n  {}\n\
             Evidence must not leave this archive silently. If these payloads were\n\
             withheld deliberately, register them (see WITHHELD.md). If they were\n\
             not, restore them before sealing.",
            diff.child_version,
            undeclared.len(),
            register_path.display(),
            undeclared.join("\n  ")
        ));
    }
    Ok(diff.removed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    fn diff_json(child_version: u64, removed: &[(&str, &str)]) -> String {
        let items: Vec<String> = removed
            .iter()
            .map(|(s, r)| {
                format!(
                    r#"{{"shard_id":"{s}","record_id":"{r}","version":1,"content_hash":"{}"}}"#,
                    "00".repeat(32)
                )
            })
            .collect();
        format!(
            r#"{{"parent_version":{},"parent_root":"{}","child_version":{child_version},
                 "child_root":"{}","added":[],"removed":[{}]}}"#,
            child_version - 1,
            "11".repeat(32),
            "22".repeat(32),
            items.join(",")
        )
    }

    fn register_json(entries: &[(&str, &str, u64)]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(s, r, v)| {
                format!(
                    r#"{{"shard_id":"{s}","record_id":"{r}","withheld_at_version":{v},
                         "date":"2026-08-09","reason":"upstream-moderation-removal"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"schema":"{SCHEMA}","withheld":[{}]}}"#,
            items.join(",")
        )
    }

    #[test]
    fn an_additive_diff_needs_no_register() {
        let d = tempfile::tempdir().unwrap();
        let diff = write(d.path(), "diff.json", &diff_json(2, &[]));
        assert_eq!(
            check_removals(&diff, &d.path().join("withheld.json")).unwrap(),
            0
        );
    }

    #[test]
    fn an_undeclared_removal_is_rejected_and_names_the_record() {
        let d = tempfile::tempdir().unwrap();
        let diff = write(
            d.path(),
            "diff.json",
            &diff_json(2, &[("post", "post/captures/000001/179.json")]),
        );
        let err = check_removals(&diff, &d.path().join("withheld.json")).unwrap_err();
        assert!(err.contains("post/post/captures/000001/179.json"), "{err}");
        assert!(err.contains("not declared"), "{err}");
    }

    #[test]
    fn a_declared_removal_at_the_right_version_passes() {
        let d = tempfile::tempdir().unwrap();
        let diff = write(
            d.path(),
            "diff.json",
            &diff_json(7, &[("post", "post/captures/000001/179.json")]),
        );
        let reg = write(
            d.path(),
            "withheld.json",
            &register_json(&[("post", "post/captures/000001/179.json", 7)]),
        );
        assert_eq!(check_removals(&diff, &reg).unwrap(), 1);
    }

    #[test]
    fn a_declaration_for_a_different_version_does_not_cover_this_removal() {
        // Otherwise one stale entry would permanently authorise removing that
        // record again, in any later version, without anyone re-declaring it.
        let d = tempfile::tempdir().unwrap();
        let diff = write(
            d.path(),
            "diff.json",
            &diff_json(9, &[("post", "post/captures/000001/179.json")]),
        );
        let reg = write(
            d.path(),
            "withheld.json",
            &register_json(&[("post", "post/captures/000001/179.json", 7)]),
        );
        let err = check_removals(&diff, &reg).unwrap_err();
        assert!(err.contains("v9 removes 1 record"), "{err}");
    }

    #[test]
    fn a_declaration_for_a_different_record_does_not_cover_this_one() {
        let d = tempfile::tempdir().unwrap();
        let diff = write(
            d.path(),
            "diff.json",
            &diff_json(3, &[("post", "post/captures/000001/179.json")]),
        );
        let reg = write(
            d.path(),
            "withheld.json",
            &register_json(&[("post", "post/captures/000001/180.json", 3)]),
        );
        assert!(check_removals(&diff, &reg).is_err());
    }

    #[test]
    fn a_broken_register_is_an_error_not_an_empty_one() {
        // A malformed register must not read as "nothing is declared" — that
        // would turn a typo into a blanket rejection, or worse, a corrupt file
        // into a silent pass if the logic were inverted.
        let d = tempfile::tempdir().unwrap();
        let diff = write(d.path(), "diff.json", &diff_json(2, &[("post", "a")]));
        let reg = write(d.path(), "withheld.json", "{ not json");
        assert!(check_removals(&diff, &reg).unwrap_err().contains("parsing"));

        let reg2 = write(
            d.path(),
            "w2.json",
            r#"{"schema":"other/v1","withheld":[]}"#,
        );
        assert!(check_removals(&diff, &reg2)
            .unwrap_err()
            .contains("expected"));
    }

    #[test]
    fn every_undeclared_removal_is_listed_not_just_the_first() {
        let d = tempfile::tempdir().unwrap();
        let diff = write(
            d.path(),
            "diff.json",
            &diff_json(
                4,
                &[
                    ("post", "post/captures/000001/1.json"),
                    ("post", "post/captures/000001/2.json"),
                    ("site", "site/captures/000001/attest.json"),
                ],
            ),
        );
        let err = check_removals(&diff, &d.path().join("withheld.json")).unwrap_err();
        assert!(err.contains("removes 3 record(s)"), "{err}");
        assert!(err.contains("post/post/captures/000001/1.json"), "{err}");
        assert!(err.contains("post/post/captures/000001/2.json"), "{err}");
        assert!(
            err.contains("site/site/captures/000001/attest.json"),
            "{err}"
        );
    }
}
