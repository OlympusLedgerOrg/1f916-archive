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
