//! Anchor coverage: every manifest is anchored, or its absence is registered.
//!
//! The anchor check iterates *bundles*. For each bundle it demands a matching
//! manifest — but that direction is the safe one. A manifest with no bundle is
//! not examined at all, because nothing enumerates it, and `--require-anchors`
//! only asserts that at least one bundle exists somewhere.
//!
//! So an unanchored manifest verifies exactly like an anchored one. That is the
//! same failure the negative controls exist to prevent, one level up: a verifier
//! that checks three signatures and silently ignores a fourth manifest looks, in
//! a terminal, identical to one that checked everything.
//!
//! This module closes that direction. Every manifest must either have a bundle
//! or appear in `unanchored.json`, naming the version and why it has no anchor.
//!
//! Two versions really are unanchorable: v1 and v2 were sealed locally before
//! the signing workflow existed. Keyless signing binds a certificate to the
//! workflow's OIDC identity at signing time, so there is no honest way to issue
//! those anchors after the fact — and forging a plausible one is exactly what
//! the identity pin exists to prevent. The register makes that a stated,
//! reviewable fact instead of a silence.
//!
//! The register is not a way to opt out of anchoring. It is checked in both
//! directions: an entry naming a version that *does* have a bundle, or one
//! naming a version that does not exist, is itself an error. A stale entry
//! cannot sit in the file pre-authorising a future anchor to go missing.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "1f916-archive/unanchored/v1";

/// One manifest version that is knowingly not anchored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnanchoredVersion {
    /// The manifest version with no Sigstore bundle.
    pub version: u64,
    /// ISO-8601 date the declaration was made.
    pub date: String,
    /// Free-text category, e.g. `sealed-before-signing-workflow-existed`.
    pub reason: String,
}

/// The register file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Register {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub unanchored: Vec<UnanchoredVersion>,
}

impl Register {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            // No register means every manifest is expected to be anchored, which
            // is the stricter reading and the right default.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    schema: SCHEMA.to_string(),
                    unanchored: Vec::new(),
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

/// Minimal view of a manifest: only the version the check needs.
#[derive(Debug, Deserialize)]
struct ManifestView {
    version: u64,
}

/// What the coverage check found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Manifests with a Sigstore bundle beside them.
    pub anchored: usize,
    /// Manifests with no bundle, each declared in the register.
    pub declared: usize,
}

/// Parse a strict ISO-8601 calendar date, `YYYY-MM-DD`.
///
/// Hand-rolled rather than pulling a date crate: this is the only date in the
/// project, and the rest of the collector already hand-rolls its argument parser
/// and JSON field reader to keep the dependency surface of an evidence tool
/// small. Rejects a well-formed but impossible date (`2026-02-30`), since the
/// point is that the field means something.
fn parse_iso_date(s: &str) -> Option<(u32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    let year: u32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

/// Days in a Gregorian month, leap years included.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// Every manifest in `manifest_dir` must have a bundle in `bundle_dir`, or be
/// declared in the register at `register_path`.
///
/// Checked in both directions: an undeclared unanchored manifest fails, and so
/// does a register entry that names an anchored or non-existent version.
pub fn check_coverage(
    manifest_dir: &Path,
    bundle_dir: &Path,
    register_path: &Path,
) -> Result<Coverage, String> {
    let mut manifests: BTreeMap<u64, String> = BTreeMap::new();

    let entries = std::fs::read_dir(manifest_dir)
        .map_err(|e| format!("reading {}: {e}", manifest_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", manifest_dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(tag) = name.strip_suffix(".json") else {
            continue;
        };
        if !tag.starts_with('v') {
            continue;
        }

        let path = entry.path();
        let bytes = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let view: ManifestView = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parsing {}: {e}", path.display()))?;

        // The filename is what pairs a manifest with its bundle, and the version
        // is what pairs it with the register. If those two disagree, a manifest
        // could be declared under one identity and anchored under another.
        let expected = format!("v{:06}", view.version);
        if tag != expected {
            return Err(format!(
                "{} declares version {} but is named {tag}, expected {expected}\n\
                 The filename pairs a manifest with its bundle and the version pairs\n\
                 it with the register; they must not disagree.",
                path.display(),
                view.version
            ));
        }
        if let Some(previous) = manifests.insert(view.version, tag.to_string()) {
            return Err(format!(
                "two manifests claim version {}: {previous} and {tag}",
                view.version
            ));
        }
    }

    if manifests.is_empty() {
        return Err(format!("no manifests in {}", manifest_dir.display()));
    }

    let register = Register::load(register_path)?;
    let mut declared: BTreeMap<u64, &UnanchoredVersion> = BTreeMap::new();
    for entry in &register.unanchored {
        // An entry is what converts a missing anchor from a silence into a
        // stated fact, so an entry that states nothing is not coverage. A blank
        // reason would satisfy the check while explaining nothing to the reader
        // it exists for.
        if entry.reason.trim().is_empty() {
            return Err(format!(
                "{} declares v{:06} with an empty reason\n\
                 A declaration is only worth anything if it says why the anchor is\n\
                 missing; an entry that names no reason records a gap without\n\
                 explaining it, which is what the register exists to prevent.",
                register_path.display(),
                entry.version
            ));
        }
        // Likewise the date: "when was this gap acknowledged" is only answerable
        // if the field is a real calendar date rather than arbitrary text.
        if parse_iso_date(&entry.date).is_none() {
            return Err(format!(
                "{} declares v{:06} with date {:?}, which is not an ISO-8601\n\
                 calendar date (YYYY-MM-DD).",
                register_path.display(),
                entry.version,
                entry.date
            ));
        }
        if declared.insert(entry.version, entry).is_some() {
            return Err(format!(
                "{} declares version {} twice",
                register_path.display(),
                entry.version
            ));
        }
    }

    let mut coverage = Coverage::default();
    let mut undeclared = Vec::new();
    let mut anchored_versions = Vec::new();

    for (version, tag) in &manifests {
        let bundle = bundle_dir.join(format!("{tag}.sigstore.json"));
        if bundle.exists() {
            coverage.anchored += 1;
            anchored_versions.push(*version);
            continue;
        }
        match declared.get(version) {
            Some(_) => coverage.declared += 1,
            None => undeclared.push(*version),
        }
    }

    if !undeclared.is_empty() {
        return Err(format!(
            "{} manifest version(s) have no Sigstore bundle and are not declared in {}:\n  {}\n\
             An unanchored manifest carries no independent timestamp, which is the\n\
             only reason this archive signs anything. Anchor it, or — if it truly\n\
             cannot be anchored — register it so the gap is a stated fact rather\n\
             than a silence.",
            undeclared.len(),
            register_path.display(),
            undeclared
                .iter()
                .map(|v| format!("v{v:06}"))
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }

    // A declaration for a version that *is* anchored is stale. Left in place it
    // would sit there pre-authorising that anchor to go missing later.
    let stale: Vec<u64> = anchored_versions
        .iter()
        .filter(|v| declared.contains_key(v))
        .copied()
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "{} declares {} version(s) unanchored that do have a bundle:\n  {}\n\
             Remove the stale entr(ies): a declaration must never outlive the gap\n\
             it describes, or it silently authorises a future anchor to vanish.",
            register_path.display(),
            stale.len(),
            stale
                .iter()
                .map(|v| format!("v{v:06}"))
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }

    let dangling: Vec<u64> = declared
        .keys()
        .filter(|v| !manifests.contains_key(v))
        .copied()
        .collect();
    if !dangling.is_empty() {
        return Err(format!(
            "{} declares {} version(s) that do not exist:\n  {}",
            register_path.display(),
            dangling.len(),
            dangling
                .iter()
                .map(|v| format!("v{v:06}"))
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }

    Ok(coverage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Fixture {
        _dir: tempfile::TempDir,
        manifests: PathBuf,
        bundles: PathBuf,
        register: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let manifests = dir.path().join("manifests");
            let bundles = dir.path().join("bundles");
            fs::create_dir_all(&manifests).unwrap();
            fs::create_dir_all(&bundles).unwrap();
            Self {
                register: dir.path().join("unanchored.json"),
                _dir: dir,
                manifests,
                bundles,
            }
        }

        /// A manifest, optionally with a bundle beside it.
        fn manifest(&self, version: u64, anchored: bool) -> &Self {
            self.manifest_named(&format!("v{version:06}"), version, anchored)
        }

        fn manifest_named(&self, tag: &str, version: u64, anchored: bool) -> &Self {
            fs::write(
                self.manifests.join(format!("{tag}.json")),
                format!(
                    r#"{{"version":{version},"manifest_root":"{}"}}"#,
                    "aa".repeat(32)
                ),
            )
            .unwrap();
            if anchored {
                fs::write(
                    self.bundles.join(format!("{tag}.sigstore.json")),
                    r#"{"logIndex":"1"}"#,
                )
                .unwrap();
            }
            self
        }

        /// A register with per-entry control over `date` and `reason`.
        fn register_raw(&self, entries: &[(u64, &str, &str)]) -> &Self {
            let items: Vec<String> = entries
                .iter()
                .map(|(v, date, reason)| {
                    format!(r#"{{"version":{v},"date":"{date}","reason":"{reason}"}}"#)
                })
                .collect();
            fs::write(
                &self.register,
                format!(
                    r#"{{"schema":"{SCHEMA}","unanchored":[{}]}}"#,
                    items.join(",")
                ),
            )
            .unwrap();
            self
        }

        fn register(&self, versions: &[u64]) -> &Self {
            let items: Vec<String> = versions
                .iter()
                .map(|v| {
                    format!(
                        r#"{{"version":{v},"date":"2026-08-09",
                             "reason":"sealed-before-signing-workflow-existed"}}"#
                    )
                })
                .collect();
            fs::write(
                &self.register,
                format!(
                    r#"{{"schema":"{SCHEMA}","unanchored":[{}]}}"#,
                    items.join(",")
                ),
            )
            .unwrap();
            self
        }

        fn check(&self) -> Result<Coverage, String> {
            check_coverage(&self.manifests, &self.bundles, &self.register)
        }
    }

    #[test]
    fn every_manifest_anchored_needs_no_register() {
        let f = Fixture::new();
        f.manifest(1, true).manifest(2, true);
        assert_eq!(
            f.check().unwrap(),
            Coverage {
                anchored: 2,
                declared: 0
            }
        );
    }

    #[test]
    fn an_undeclared_unanchored_manifest_is_rejected_and_names_the_version() {
        // The negative control for this whole module: before it existed, this
        // archive state passed every check.
        let f = Fixture::new();
        f.manifest(1, true).manifest(2, false).manifest(3, true);
        let err = f.check().unwrap_err();
        assert!(err.contains("v000002"), "{err}");
        assert!(err.contains("not declared"), "{err}");
        assert!(
            !err.contains("v000001"),
            "anchored versions must not be named: {err}"
        );
    }

    #[test]
    fn a_declared_unanchored_manifest_passes() {
        let f = Fixture::new();
        f.manifest(1, false).manifest(2, false).manifest(3, true);
        f.register(&[1, 2]);
        assert_eq!(
            f.check().unwrap(),
            Coverage {
                anchored: 1,
                declared: 2
            }
        );
    }

    #[test]
    fn every_undeclared_version_is_listed_not_just_the_first() {
        let f = Fixture::new();
        f.manifest(1, false).manifest(2, false).manifest(3, false);
        let err = f.check().unwrap_err();
        assert!(err.contains("3 manifest version(s)"), "{err}");
        for tag in ["v000001", "v000002", "v000003"] {
            assert!(err.contains(tag), "{err}");
        }
    }

    #[test]
    fn a_declaration_does_not_cover_a_different_version() {
        let f = Fixture::new();
        f.manifest(1, false).manifest(2, false);
        f.register(&[1]);
        let err = f.check().unwrap_err();
        assert!(err.contains("v000002"), "{err}");
    }

    #[test]
    fn a_stale_declaration_for_an_anchored_version_is_rejected() {
        // Otherwise the entry sits there indefinitely, and the day that bundle
        // goes missing the check that should have caught it waves it through.
        let f = Fixture::new();
        f.manifest(1, true);
        f.register(&[1]);
        let err = f.check().unwrap_err();
        assert!(err.contains("do have a bundle"), "{err}");
        assert!(err.contains("v000001"), "{err}");
    }

    #[test]
    fn a_declaration_for_a_version_that_does_not_exist_is_rejected() {
        let f = Fixture::new();
        f.manifest(1, true);
        f.register(&[9]);
        let err = f.check().unwrap_err();
        assert!(err.contains("do not exist"), "{err}");
        assert!(err.contains("v000009"), "{err}");
    }

    #[test]
    fn a_manifest_named_inconsistently_with_its_version_is_rejected() {
        // A manifest hiding under another version's filename could be declared
        // under one identity and anchored under another.
        let f = Fixture::new();
        f.manifest_named("v000007", 3, true);
        let err = f.check().unwrap_err();
        assert!(err.contains("declares version 3"), "{err}");
        assert!(err.contains("v000007"), "{err}");
    }

    #[test]
    fn a_broken_register_is_an_error_not_an_empty_one() {
        let f = Fixture::new();
        f.manifest(1, false);
        fs::write(&f.register, "{ not json").unwrap();
        assert!(f.check().unwrap_err().contains("parsing"));

        fs::write(&f.register, r#"{"schema":"other/v1","unanchored":[]}"#).unwrap();
        assert!(f.check().unwrap_err().contains("expected"));
    }

    #[test]
    fn a_missing_register_means_everything_must_be_anchored() {
        let f = Fixture::new();
        f.manifest(1, false);
        let err = f.check().unwrap_err();
        assert!(err.contains("not declared"), "{err}");

        let g = Fixture::new();
        g.manifest(1, true);
        assert_eq!(g.check().unwrap().anchored, 1);
    }

    #[test]
    fn an_empty_manifest_directory_is_an_error() {
        let f = Fixture::new();
        assert!(f.check().unwrap_err().contains("no manifests"));
    }

    #[test]
    fn a_declaration_with_a_blank_reason_is_not_coverage() {
        // The register turns a silence into a stated fact. An entry that states
        // nothing would satisfy the check while explaining nothing.
        // `\\t` is the JSON escape, so the file on disk holds a real tab.
        for blank in ["", "   ", "\\t"] {
            let f = Fixture::new();
            f.manifest(1, false);
            f.register_raw(&[(1, "2026-08-09", blank)]);
            let err = f.check().unwrap_err();
            assert!(err.contains("empty reason"), "{blank:?}: {err}");
            assert!(err.contains("v000001"), "{blank:?}: {err}");
        }
    }

    #[test]
    fn a_declaration_whose_date_is_not_a_date_is_rejected() {
        for bad in ["x", "2026-8-9", "09-08-2026", "2026-08-09T00:00:00Z", ""] {
            let f = Fixture::new();
            f.manifest(1, false);
            f.register_raw(&[(1, bad, "sealed-before-signing-workflow-existed")]);
            let err = f.check().unwrap_err();
            assert!(err.contains("ISO-8601"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_declaration_with_an_impossible_date_is_rejected() {
        // Well-formed but not a real day: the field is meant to answer "when was
        // this gap acknowledged", which it cannot if it never happened.
        for bad in ["2026-02-30", "2026-13-01", "2026-00-10", "2026-04-31"] {
            let f = Fixture::new();
            f.manifest(1, false);
            f.register_raw(&[(1, bad, "reason")]);
            let err = f.check().unwrap_err();
            assert!(err.contains("ISO-8601"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_well_formed_declaration_still_passes() {
        let f = Fixture::new();
        f.manifest(1, false);
        f.register_raw(&[(1, "2024-02-29", "leap day is a real day")]);
        assert_eq!(f.check().unwrap().declared, 1);
    }

    #[test]
    fn iso_dates_accept_real_days_and_reject_impossible_ones() {
        assert_eq!(parse_iso_date("2026-08-09"), Some((2026, 8, 9)));
        assert_eq!(parse_iso_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_iso_date("2000-02-29"), Some((2000, 2, 29)));
        // 1900 is divisible by 4 but not a leap year: divisible by 100, not 400.
        assert_eq!(parse_iso_date("1900-02-29"), None);
        assert_eq!(parse_iso_date("2026-02-29"), None);
        assert_eq!(parse_iso_date("2026-08-09 "), None);
        assert_eq!(parse_iso_date("2026/08/09"), None);
    }

    #[test]
    fn a_duplicate_declaration_is_rejected() {
        let f = Fixture::new();
        f.manifest(1, false);
        fs::write(
            &f.register,
            format!(
                r#"{{"schema":"{SCHEMA}","unanchored":[
                     {{"version":1,"date":"2026-08-09","reason":"a"}},
                     {{"version":1,"date":"2026-08-09","reason":"b"}}]}}"#
            ),
        )
        .unwrap();
        assert!(f.check().unwrap_err().contains("twice"));
    }

    #[test]
    fn non_manifest_files_in_the_directory_are_ignored() {
        let f = Fixture::new();
        f.manifest(1, true);
        fs::write(f.manifests.join("README.md"), "notes").unwrap();
        fs::write(f.manifests.join("latest.json"), "{}").unwrap();
        assert_eq!(f.check().unwrap().anchored, 1);
    }
}
