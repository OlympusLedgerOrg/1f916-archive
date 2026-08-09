//! Committed collector state, and the ordering rule that makes a rerun safe.
//!
//! The cursor is the only thing in this system that can *lose* evidence: advance
//! it past rows whose packets were not durably written and those rows are never
//! offered again. So the run order is fixed and one-directional:
//!
//! ```text
//! fetch -> write packets (create_new + fsync) -> save state (tmp + fsync + rename)
//! ```
//!
//! A crash at any point leaves the previous state file intact, so the next run
//! refetches from the last committed cursor. That may re-capture a boundary row —
//! which is harmless, because it lands in a *new* capture sequence and overwrites
//! nothing.
//!
//! The absent-id cache deserves its own note. Caching "id N is a 404" is only
//! sound below the highest id ever seen present: above that watermark a 404 means
//! "not created yet", and caching it would blind the sweep to every future post.
//! [`CollectorState::record_absent`] enforces that.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Everything the collector must remember between runs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CollectorState {
    /// Schema tag, so a future format change is a loud failure.
    #[serde(default = "schema")]
    pub schema: String,
    /// `/api/changes` cursor. Advanced to `next_since`, never to `now`.
    #[serde(default)]
    pub changes_cursor: i64,
    /// Highest capture sequence known to have been committed.
    #[serde(default)]
    pub last_capture_seq: u64,
    /// Post ids for which a full `/api/post/{id}` packet exists.
    #[serde(default)]
    pub captured_post_ids: BTreeSet<u64>,
    /// Post ids confirmed absent (HTTP 404) *below* `max_present_post_id`.
    #[serde(default)]
    pub absent_post_ids: BTreeSet<u64>,
    /// Highest post id ever observed to exist. The watermark below which a 404
    /// is a permanent fact rather than a "not yet".
    #[serde(default)]
    pub max_present_post_id: u64,
    /// Rotation offset for the bounded re-fetch sweep, so successive runs cover
    /// different parts of the corpus.
    #[serde(default)]
    pub refetch_offset: u64,
    /// Unix milliseconds of the last completed run.
    #[serde(default)]
    pub last_run_ms: u64,
    /// Completed run count.
    #[serde(default)]
    pub runs: u64,
}

fn schema() -> String {
    SCHEMA.to_string()
}

pub const SCHEMA: &str = "1f916-archive/collector-state/v1";

impl CollectorState {
    /// Load state, or return a fresh zero state if the file does not exist.
    ///
    /// A corrupt or unknown-schema file is an error, never a silent reset: a
    /// silent reset would set the cursor to 0 and re-drain the entire history
    /// into a fresh capture sequence.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    schema: schema(),
                    ..Self::default()
                })
            }
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parsing {}: {e}", path.display()))?;
        if state.schema != SCHEMA {
            return Err(format!(
                "{} has schema {:?}, expected {SCHEMA}",
                path.display(),
                state.schema
            ));
        }
        Ok(state)
    }

    /// Atomically replace the state file: write a temp file, fsync it, rename
    /// over the target, then fsync the directory.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let dir = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let tmp: PathBuf = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        {
            let mut f =
                fs::File::create(&tmp).map_err(|e| format!("creating {}: {e}", tmp.display()))?;
            f.write_all(&bytes)
                .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
            f.write_all(b"\n").map_err(|e| e.to_string())?;
            f.sync_all()
                .map_err(|e| format!("fsync {}: {e}", tmp.display()))?;
        }
        fs::rename(&tmp, path)
            .map_err(|e| format!("renaming {} -> {}: {e}", tmp.display(), path.display()))?;
        let _ = crate::packet::sync_dir(dir);
        Ok(())
    }

    /// Record that a post id was captured. Also raises the present-watermark and
    /// clears any stale absence, since an id that answered cannot be absent.
    pub fn record_present(&mut self, id: u64) {
        self.captured_post_ids.insert(id);
        self.absent_post_ids.remove(&id);
        self.max_present_post_id = self.max_present_post_id.max(id);
    }

    /// Record a 404 for `id`.
    ///
    /// Only cached below `max_present_post_id`. Above that watermark the id may
    /// simply not exist yet, and remembering the 404 would permanently hide the
    /// post that later occupies it.
    pub fn record_absent(&mut self, id: u64) {
        if id < self.max_present_post_id {
            self.absent_post_ids.insert(id);
        }
    }

    /// Whether an id still needs a first full fetch.
    pub fn needs_capture(&self, id: u64) -> bool {
        !self.captured_post_ids.contains(&id) && !self.absent_post_ids.contains(&id)
    }

    /// Ids in `[1, max_present_post_id]` that are neither captured nor known
    /// absent — the gap set `/api/changes` cannot reach, because it omits every
    /// post with a non-null `mod_state`.
    pub fn gap_ids(&self) -> Vec<u64> {
        (1..=self.max_present_post_id)
            .filter(|id| self.needs_capture(*id))
            .collect()
    }

    /// Take `count` post ids for the bounded re-fetch rotation, advancing the
    /// offset so successive runs cover different records.
    pub fn take_rotation(&mut self, count: usize) -> Vec<u64> {
        let all: Vec<u64> = self.captured_post_ids.iter().copied().collect();
        if all.is_empty() || count == 0 {
            return Vec::new();
        }
        let n = count.min(all.len());
        let start = (self.refetch_offset % all.len() as u64) as usize;
        let picked: Vec<u64> = all.iter().cycle().skip(start).take(n).copied().collect();
        self.refetch_offset = (start as u64 + n as u64) % all.len() as u64;
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_a_fresh_state() {
        let tmp = tempfile::tempdir().unwrap();
        let s = CollectorState::load(&tmp.path().join("nope.json")).unwrap();
        assert_eq!(s.changes_cursor, 0);
        assert_eq!(s.schema, SCHEMA);
    }

    #[test]
    fn corrupt_state_is_an_error_not_a_silent_reset() {
        // A silent reset would re-drain the entire history from since=0.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("state.json");
        fs::write(&p, b"{ this is not json").unwrap();
        assert!(CollectorState::load(&p).unwrap_err().contains("parsing"));

        fs::write(&p, br#"{"schema":"something/else/v9"}"#).unwrap();
        let err = CollectorState::load(&p).unwrap_err();
        assert!(err.contains("expected"), "unexpected message: {err}");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("state.json");
        let mut s = CollectorState {
            schema: schema(),
            changes_cursor: 1786033222133,
            last_capture_seq: 3,
            ..Default::default()
        };
        s.record_present(506);
        s.save(&p).unwrap();
        let back = CollectorState::load(&p).unwrap();
        assert_eq!(back.changes_cursor, 1786033222133);
        assert_eq!(back.last_capture_seq, 3);
        assert!(back.captured_post_ids.contains(&506));
        // The temp file must not survive a successful save.
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn absence_is_cached_only_below_the_present_watermark() {
        let mut s = CollectorState::default();
        s.record_present(100);
        // 50 is a real gap: it existed in the id space and answered 404.
        s.record_absent(50);
        assert!(s.absent_post_ids.contains(&50));
        // 101 has simply not been created yet. Caching that 404 would hide the
        // post that eventually takes the id.
        s.record_absent(101);
        assert!(!s.absent_post_ids.contains(&101));
        assert!(s.needs_capture(101));
        // Exactly at the watermark is also "not yet" — the watermark itself is
        // known present, so anything >= it is out of the settled range.
        s.record_absent(100);
        assert!(!s.absent_post_ids.contains(&100));
    }

    #[test]
    fn a_previously_absent_id_that_answers_is_no_longer_absent() {
        let mut s = CollectorState::default();
        s.record_present(10);
        s.record_absent(5);
        assert!(!s.needs_capture(5));
        s.record_present(5);
        assert!(!s.absent_post_ids.contains(&5));
        assert!(s.captured_post_ids.contains(&5));
    }

    #[test]
    fn gap_ids_are_exactly_the_unreachable_moderated_posts() {
        // Mirrors the live finding: ids 1..=10 exist, 4 and 7 never appear in
        // /api/changes because they are moderated. The sweep must surface them.
        let mut s = CollectorState::default();
        for id in [1, 2, 3, 5, 6, 8, 9, 10] {
            s.record_present(id);
        }
        assert_eq!(s.gap_ids(), vec![4, 7]);
        s.record_absent(4); // hard-deleted: 404
        s.record_present(7); // moderated but still served
        assert!(s.gap_ids().is_empty());
    }

    #[test]
    fn rotation_advances_and_wraps_without_repeating_within_a_pass() {
        let mut s = CollectorState::default();
        for id in 1..=5 {
            s.record_present(id);
        }
        let a = s.take_rotation(2);
        let b = s.take_rotation(2);
        let c = s.take_rotation(2);
        assert_eq!(a, vec![1, 2]);
        assert_eq!(b, vec![3, 4]);
        assert_eq!(c, vec![5, 1], "rotation must wrap");
        // Asking for more than exist yields each exactly once.
        let d = s.take_rotation(50);
        assert_eq!(d.len(), 5);
        let unique: BTreeSet<u64> = d.into_iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn rotation_on_an_empty_corpus_is_empty() {
        let mut s = CollectorState::default();
        assert!(s.take_rotation(10).is_empty());
        s.record_present(1);
        assert!(s.take_rotation(0).is_empty());
    }
}
