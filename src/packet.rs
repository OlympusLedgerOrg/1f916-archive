//! Immutable capture packets.
//!
//! An evidence archive that can overwrite a file is not an evidence archive.
//! Two rules are enforced here mechanically rather than by convention:
//!
//! 1. **Nothing is ever overwritten.** Files are created with `create_new`, so a
//!    second write to an existing path fails loudly instead of destroying the
//!    earlier capture. A rerun after a crash lands in a fresh capture sequence.
//!
//! 2. **No upstream byte can influence a path.** A packet name is either a fixed
//!    `&'static str` chosen at the call site or a locally formatted integer.
//!    [`PacketName`] has no variant that accepts caller-supplied text, so there
//!    is no way to spell a path-traversing name even deliberately.
//!
//! Layout, with the capture sequence allocated per run:
//!
//! ```text
//! archive/post/captures/000001/506.json
//! archive/post/captures/000001/506.meta.json
//! archive/site/captures/000001/attest.json
//! archive/changes/captures/000001/page-0001.json
//! ```
//!
//! The `.meta.json` sidecar holds the collector's observations — request URL,
//! status, content type, timings, and the collector's own BLAKE3 of the body.
//! Those observations are never merged into the response body, which is stored
//! byte-for-byte as received.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The manifest shards this archive writes. A shard is the top-level directory,
/// which is what `olympus build --shard-from-subdir` keys on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shard {
    /// Full post + nested comments, from `/api/post/{id}`.
    Post,
    /// Raw `/api/changes` discovery pages — the only place a comment body is
    /// seen at discovery time, before any later moderation redacts it upstream.
    Changes,
    /// The complete ascending `/api/events` log, including its hash chain.
    Events,
    /// `/treasury`.
    Treasury,
    /// `/` front door and `/api/attest`, `/api/official`, `/api/docket`.
    Site,
}

impl Shard {
    pub fn dir(self) -> &'static str {
        match self {
            Shard::Post => "post",
            Shard::Changes => "changes",
            Shard::Events => "events",
            Shard::Treasury => "treasury",
            Shard::Site => "site",
        }
    }

    pub const ALL: [Shard; 5] = [
        Shard::Post,
        Shard::Changes,
        Shard::Events,
        Shard::Treasury,
        Shard::Site,
    ];
}

/// A packet file name. Every variant is either a compile-time constant or a
/// locally generated integer; none can carry upstream text.
#[derive(Clone, Copy, Debug)]
pub enum PacketName {
    /// A constant chosen at the call site, e.g. `"attest.json"`.
    Fixed(&'static str),
    /// `{id}.json` for an upstream numeric id.
    NumericId(u64),
    /// `page-{n:04}.json` for a locally counted page within a run.
    Page(u32),
}

impl PacketName {
    fn file_name(self) -> String {
        match self {
            PacketName::Fixed(s) => s.to_string(),
            PacketName::NumericId(id) => format!("{id}.json"),
            PacketName::Page(n) => format!("page-{n:04}.json"),
        }
    }

    fn meta_name(self) -> String {
        let base = self.file_name();
        // Split on the final dot so `506.json` -> `506.meta.json`, never on an
        // earlier one.
        match base.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}.meta.{ext}"),
            None => format!("{base}.meta"),
        }
    }
}

/// Errors from the packet layer. `AlreadyExists` is called out separately
/// because it means the archive was about to lose evidence.
#[derive(Debug)]
pub enum PacketError {
    AlreadyExists(PathBuf),
    Io(String),
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::AlreadyExists(p) => write!(
                f,
                "refusing to overwrite an existing capture packet: {}",
                p.display()
            ),
            PacketError::Io(m) => write!(f, "{m}"),
        }
    }
}

type Result<T> = std::result::Result<T, PacketError>;

fn io<E: std::fmt::Display>(ctx: &str) -> impl Fn(E) -> PacketError + '_ {
    move |e| PacketError::Io(format!("{ctx}: {e}"))
}

/// A single run's capture sequence, shared across shards.
pub struct CaptureRun {
    root: PathBuf,
    seq: u64,
    written: Vec<PathBuf>,
}

/// Sidecar metadata recorded alongside a packet.
pub struct PacketMeta<'a> {
    pub url: &'a str,
    pub status: u16,
    pub content_type: &'a str,
    pub server_date: Option<&'a str>,
    pub fetched_at_ms: u64,
    pub elapsed_ms: u64,
    pub attempts: u32,
}

impl CaptureRun {
    /// Open the next capture sequence under `root`.
    ///
    /// The sequence is one past the highest directory that already exists in any
    /// shard, so a run that crashed halfway can never be resumed *into* — its
    /// partial packets stay exactly as captured and the next run starts a fresh
    /// directory. `floor` lets the caller additionally refuse to reuse a number
    /// recorded in committed state.
    pub fn open(root: &Path, floor: u64) -> Result<Self> {
        let mut max = floor;
        for shard in Shard::ALL {
            let captures = root.join(shard.dir()).join("captures");
            let Ok(entries) = fs::read_dir(&captures) else {
                continue;
            };
            for entry in entries {
                let entry = entry.map_err(io("reading capture directory"))?;
                if !entry.file_type().map_err(io("stat"))?.is_dir() {
                    continue;
                }
                if let Some(n) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    max = max.max(n);
                }
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            seq: max + 1,
            written: Vec::new(),
        })
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Paths written by this run, in write order.
    pub fn written(&self) -> &[PathBuf] {
        &self.written
    }

    fn dir_for(&self, shard: Shard) -> PathBuf {
        self.root
            .join(shard.dir())
            .join("captures")
            .join(format!("{:06}", self.seq))
    }

    /// Write a packet and its sidecar. Fails rather than overwriting.
    pub fn write(
        &mut self,
        shard: Shard,
        name: PacketName,
        body: &[u8],
        meta: &PacketMeta<'_>,
    ) -> Result<PathBuf> {
        let dir = self.dir_for(shard);
        fs::create_dir_all(&dir).map_err(io("creating capture directory"))?;

        let path = dir.join(name.file_name());
        create_new_and_sync(&path, body)?;
        self.written.push(path.clone());

        let meta_path = dir.join(name.meta_name());
        let meta_json = render_meta(shard, self.seq, name, body, meta);
        create_new_and_sync(&meta_path, meta_json.as_bytes())?;
        self.written.push(meta_path);

        Ok(path)
    }
}

/// Create a file that must not already exist, write it, and fsync it.
///
/// `create_new` is the whole point: it is the kernel telling us the path was
/// free, not a racy `exists()` check we performed a moment earlier.
fn create_new_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(PacketError::AlreadyExists(path.to_path_buf()))
        }
        Err(e) => return Err(PacketError::Io(format!("creating {}: {e}", path.display()))),
    };
    f.write_all(bytes)
        .map_err(io(&format!("writing {}", path.display())))?;
    f.sync_all()
        .map_err(io(&format!("fsync {}", path.display())))?;
    Ok(())
}

/// fsync a directory so its entries survive a crash. A no-op on Windows, where
/// directories cannot be opened for synchronisation.
pub fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Render the sidecar. Written by hand rather than via `serde_json` so the
/// field order is stable and obvious, and so it is plain that no upstream text
/// is interpolated: every value below is a number, a fixed string, or a
/// JSON-escaped locally observed header.
fn render_meta(
    shard: Shard,
    seq: u64,
    name: PacketName,
    body: &[u8],
    meta: &PacketMeta<'_>,
) -> String {
    let date = match meta.server_date {
        Some(d) => format!("\"{}\"", escape(d)),
        None => "null".to_string(),
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"1f916-archive/capture-meta/v1\",\n",
            "  \"shard\": \"{}\",\n",
            "  \"capture_seq\": {},\n",
            "  \"packet\": \"{}\",\n",
            "  \"request_url\": \"{}\",\n",
            "  \"http_status\": {},\n",
            "  \"content_type\": \"{}\",\n",
            "  \"server_date_claimed\": {},\n",
            "  \"fetched_at_ms\": {},\n",
            "  \"elapsed_ms\": {},\n",
            "  \"attempts\": {},\n",
            "  \"body_bytes\": {},\n",
            "  \"body_blake3\": \"{}\",\n",
            "  \"collector\": \"{}\"\n",
            "}}\n"
        ),
        shard.dir(),
        seq,
        escape(&name.file_name()),
        escape(meta.url),
        meta.status,
        escape(meta.content_type),
        date,
        meta.fetched_at_ms,
        meta.elapsed_ms,
        meta.attempts,
        body.len(),
        blake3::hash(body).to_hex(),
        escape(crate::http::USER_AGENT),
    )
}

/// Minimal JSON string escaping for the sidecar's few locally sourced strings.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta<'a>(url: &'a str) -> PacketMeta<'a> {
        PacketMeta {
            url,
            status: 200,
            content_type: "application/json",
            server_date: Some("Sun, 09 Aug 2026 18:30:49 GMT"),
            fetched_at_ms: 1786300249518,
            elapsed_ms: 42,
            attempts: 1,
        }
    }

    #[test]
    fn packet_names_cannot_express_traversal() {
        // There is no `PacketName` variant that accepts caller text, so the only
        // names constructible are these shapes. Assert their exact spelling.
        assert_eq!(PacketName::NumericId(506).file_name(), "506.json");
        assert_eq!(PacketName::Page(1).file_name(), "page-0001.json");
        assert_eq!(PacketName::Fixed("attest.json").file_name(), "attest.json");
        for name in [
            PacketName::NumericId(u64::MAX),
            PacketName::Page(u32::MAX),
            PacketName::Fixed("front-door.txt"),
        ] {
            let f = name.file_name();
            assert!(!f.contains('/'), "{f}");
            assert!(!f.contains('\\'), "{f}");
            assert!(!f.contains(".."), "{f}");
            assert!(!f.contains(':'), "{f}");
        }
    }

    #[test]
    fn meta_name_splits_on_the_final_dot() {
        assert_eq!(PacketName::NumericId(506).meta_name(), "506.meta.json");
        assert_eq!(
            PacketName::Fixed("front-door.txt").meta_name(),
            "front-door.meta.txt"
        );
        assert_eq!(PacketName::Fixed("noext").meta_name(), "noext.meta");
    }

    #[test]
    fn writes_a_packet_and_a_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let mut run = CaptureRun::open(tmp.path(), 0).unwrap();
        assert_eq!(run.seq(), 1);
        let p = run
            .write(
                Shard::Post,
                PacketName::NumericId(506),
                b"{\"post\":1}",
                &meta("https://1f916.ai/api/post/506"),
            )
            .unwrap();
        assert!(p.ends_with("506.json"));
        assert_eq!(fs::read(&p).unwrap(), b"{\"post\":1}");
        let side = p.with_file_name("506.meta.json");
        let text = fs::read_to_string(&side).unwrap();
        assert!(text.contains("\"capture_seq\": 1"));
        assert!(text.contains("\"body_bytes\": 10"));
        assert!(text.contains(&blake3::hash(b"{\"post\":1}").to_hex().to_string()));
        // The sidecar must parse as JSON, or the manifest layer stores garbage.
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["shard"], "post");
    }

    #[test]
    fn refuses_to_overwrite_an_existing_packet() {
        let tmp = tempfile::tempdir().unwrap();
        let mut run = CaptureRun::open(tmp.path(), 0).unwrap();
        run.write(
            Shard::Post,
            PacketName::NumericId(1),
            b"first",
            &meta("https://1f916.ai/api/post/1"),
        )
        .unwrap();
        let again = run.write(
            Shard::Post,
            PacketName::NumericId(1),
            b"second",
            &meta("https://1f916.ai/api/post/1"),
        );
        // Not merely "an error": specifically the overwrite guard, and the
        // original bytes must still be on disk.
        match again {
            Err(PacketError::AlreadyExists(p)) => assert!(p.ends_with("1.json")),
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
        let path = tmp.path().join("post/captures/000001/1.json");
        assert_eq!(fs::read(path).unwrap(), b"first");
    }

    #[test]
    fn a_new_run_never_reuses_a_sequence() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r1 = CaptureRun::open(tmp.path(), 0).unwrap();
        r1.write(
            Shard::Post,
            PacketName::NumericId(1),
            b"a",
            &meta("https://1f916.ai/api/post/1"),
        )
        .unwrap();
        // Simulate a crash: r1 is dropped mid-run, having written one packet.
        let r2 = CaptureRun::open(tmp.path(), 0).unwrap();
        assert_eq!(r2.seq(), 2, "a rerun must start a fresh sequence");

        // The sequence floor from committed state is also honoured, so a run
        // whose directory listing is somehow incomplete still cannot collide.
        let r3 = CaptureRun::open(tmp.path(), 9).unwrap();
        assert_eq!(r3.seq(), 10);
    }

    #[test]
    fn sequence_max_is_taken_across_all_shards() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("post/captures/000001")).unwrap();
        fs::create_dir_all(tmp.path().join("site/captures/000007")).unwrap();
        assert_eq!(CaptureRun::open(tmp.path(), 0).unwrap().seq(), 8);
    }

    #[test]
    fn a_repeat_capture_in_a_later_sequence_is_allowed_and_preserves_both() {
        // Re-fetching the same post is the whole point of the rotation sweep:
        // the pair of packets is the evidence of an upstream state change.
        let tmp = tempfile::tempdir().unwrap();
        let mut r1 = CaptureRun::open(tmp.path(), 0).unwrap();
        r1.write(
            Shard::Post,
            PacketName::NumericId(506),
            b"before moderation",
            &meta("https://1f916.ai/api/post/506"),
        )
        .unwrap();
        let mut r2 = CaptureRun::open(tmp.path(), 0).unwrap();
        r2.write(
            Shard::Post,
            PacketName::NumericId(506),
            b"after moderation",
            &meta("https://1f916.ai/api/post/506"),
        )
        .unwrap();
        assert_eq!(
            fs::read(tmp.path().join("post/captures/000001/506.json")).unwrap(),
            b"before moderation"
        );
        assert_eq!(
            fs::read(tmp.path().join("post/captures/000002/506.json")).unwrap(),
            b"after moderation"
        );
    }

    #[test]
    fn body_bytes_are_stored_verbatim_including_invalid_utf8() {
        // The archive commits to what it received. A lossy round-trip through
        // `String` would change the hash and break that claim.
        let tmp = tempfile::tempdir().unwrap();
        let mut run = CaptureRun::open(tmp.path(), 0).unwrap();
        let raw = b"\xff\xfe not utf-8 \x00 \x80";
        let p = run
            .write(
                Shard::Site,
                PacketName::Fixed("front-door.txt"),
                raw,
                &meta("https://1f916.ai/"),
            )
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), raw);
    }

    #[test]
    fn sidecar_escapes_control_characters_in_observed_headers() {
        // Header values are locally observed but still remote-controlled; a raw
        // newline would produce an unparseable sidecar.
        let s = escape("a\"b\\c\nd\te\u{1}");
        assert_eq!(s, "a\\\"b\\\\c\\nd\\te\\u0001");
        let json = format!("{{\"v\":\"{s}\"}}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["v"], "a\"b\\c\nd\te\u{1}");
    }
}
