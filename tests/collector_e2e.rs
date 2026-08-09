//! End-to-end collector behaviour against a local, scripted HTTP server.
//!
//! The unit tests cover each layer in isolation. These cover the properties that
//! only exist when the layers are composed, and that the archive's whole claim
//! rests on:
//!
//! * a completed run advances the cursor to `next_since`,
//! * a run that fails partway does **not** advance it,
//! * a rerun after that failure re-captures into a *new* sequence and destroys
//!   nothing,
//! * the id sweep reaches posts `/api/changes` never mentions.
//!
//! The server is deliberately hand-rolled: a scripted socket is the only way to
//! make a failure happen exactly where a test needs it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// A canned response.
#[derive(Clone)]
struct Canned {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Canned {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into(),
        }
    }
    fn text(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
        }
    }
    fn not_found() -> Self {
        Self {
            status: 404,
            content_type: "application/json",
            body: r#"{"error":"not found"}"#.into(),
        }
    }
    fn server_error() -> Self {
        Self {
            status: 500,
            content_type: "application/json",
            body: r#"{"error":"boom"}"#.into(),
        }
    }
}

struct Server {
    base: String,
    hits: Arc<AtomicUsize>,
}

/// Serve `routes` (path-with-query -> response) until the process ends.
/// Any path not in the map answers 404, which is what the id sweep expects.
fn serve(routes: HashMap<String, Canned>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let routes = Arc::new(Mutex::new(routes));
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = hits.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = routes.clone();
            hits_thread.fetch_add(1, Ordering::SeqCst);
            if let Some(target) = read_request_target(&mut stream) {
                let resp = routes
                    .lock()
                    .unwrap()
                    .get(&target)
                    .cloned()
                    .unwrap_or_else(Canned::not_found);
                let _ = write_response(&mut stream, &resp);
            }
        }
    });

    Server {
        base: format!("http://127.0.0.1:{port}"),
        hits,
    }
}

fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1)?.to_string();
    // Drain headers so the client sees a clean response.
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).ok()? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    Some(target)
}

fn write_response(stream: &mut TcpStream, r: &Canned) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        r.status,
        r.content_type,
        r.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(r.body.as_bytes())?;
    stream.flush()
}

/// Every endpoint the collector touches before it gets to `/api/changes`.
fn site_routes() -> HashMap<String, Canned> {
    let mut r = HashMap::new();
    r.insert("/api/attest".into(), Canned::json(r#"{"ok":true}"#));
    r.insert("/".into(), Canned::text("front door policy text"));
    r.insert("/api/official".into(), Canned::json("{}"));
    r.insert("/api/docket".into(), Canned::json("{}"));
    r.insert("/treasury".into(), Canned::json(r#"{"booked_cents":0}"#));
    r.insert(
        "/api/events?since=0".into(),
        Canned::json(r#"{"has_more":false,"events":[]}"#),
    );
    r
}

fn post_body(id: u64) -> String {
    // Includes the sort of hostile text the real API carries.
    format!(
        r#"{{"post":{{"id":{id},"title":"../../etc/passwd","body":"$(whoami)"}},"comments":[]}}"#
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    archive: std::path::PathBuf,
    state: std::path::PathBuf,
    bin: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    let state = dir.path().join("state/collector-state.json");
    Fixture {
        archive,
        state,
        bin: bin_path(),
        _dir: dir,
    }
}

fn bin_path() -> std::path::PathBuf {
    // `target/<profile>/deps/<test>` -> `target/<profile>/f916-collect`
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(format!("f916-collect{}", std::env::consts::EXE_SUFFIX))
}

impl Fixture {
    fn collect(&self, base: &str, extra: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("collect")
            .arg("--root")
            .arg(&self.archive)
            .arg("--state")
            .arg(&self.state)
            .arg("--base")
            .arg(base)
            .arg("--min-interval-ms")
            .arg("0")
            // One attempt: these tests script a deterministic failure, and a
            // retry budget would only add jittered sleep to the suite.
            .arg("--max-attempts")
            .arg("1");
        cmd.args(extra);
        cmd.output().expect("run collector")
    }

    fn cursor(&self) -> i64 {
        let s = std::fs::read_to_string(&self.state).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        v["changes_cursor"].as_i64().unwrap()
    }

    fn packets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.archive.clone()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(
                        p.strip_prefix(&self.archive)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        out.sort();
        out
    }
}

#[test]
fn a_complete_run_advances_the_cursor_and_captures_every_discovered_post() {
    let mut routes = site_routes();
    routes.insert(
        "/api/changes?since=0".into(),
        Canned::json(
            r#"{"next_since":500,"has_more":false,
                "posts":[{"id":1},{"id":2}],
                "comments":[{"post_id":1}]}"#,
        ),
    );
    routes.insert("/api/post/1".into(), Canned::json(post_body(1)));
    routes.insert("/api/post/2".into(), Canned::json(post_body(2)));
    let server = serve(routes);

    let fx = fixture();
    let out = fx.collect(&server.base, &["--no-sweep", "--no-refetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(fx.cursor(), 500, "cursor must advance to next_since");
    let packets = fx.packets();
    for want in [
        "post/captures/000001/1.json",
        "post/captures/000001/2.json",
        "changes/captures/000001/page-0001.json",
        "site/captures/000001/attest.json",
        "site/captures/000001/front-door.txt",
        "treasury/captures/000001/treasury.json",
        "events/captures/000001/page-0001.json",
    ] {
        assert!(
            packets.contains(&want.to_string()),
            "missing {want}\n{packets:#?}"
        );
    }
    // The hostile title in the body must be inert: no path anywhere reflects it.
    for p in &packets {
        assert!(
            !p.contains(".."),
            "path traversal reached the filesystem: {p}"
        );
        assert!(!p.contains("passwd"), "upstream text reached a path: {p}");
    }
    // And the bytes are stored verbatim, hostile text and all.
    let stored = std::fs::read_to_string(fx.archive.join("post/captures/000001/1.json")).unwrap();
    assert_eq!(stored, post_body(1));
    assert!(server.hits.load(Ordering::SeqCst) > 0);
}

#[test]
fn a_failed_post_fetch_holds_the_cursor_and_the_rerun_loses_nothing() {
    // Post 2 is broken. The run must capture what it can, keep the cursor, and
    // leave the successful packets in place.
    let mut routes = site_routes();
    routes.insert(
        "/api/changes?since=0".into(),
        Canned::json(
            r#"{"next_since":900,"has_more":false,
                "posts":[{"id":1},{"id":2}],"comments":[]}"#,
        ),
    );
    routes.insert("/api/post/1".into(), Canned::json(post_body(1)));
    routes.insert("/api/post/2".into(), Canned::server_error());
    let server = serve(routes);

    let fx = fixture();
    let out = fx.collect(&server.base, &["--no-sweep", "--no-refetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("held at 0"),
        "cursor should not advance:\n{stdout}"
    );
    assert_eq!(
        fx.cursor(),
        0,
        "a run that lost a discovered post must not advance"
    );
    assert!(fx
        .packets()
        .contains(&"post/captures/000001/1.json".to_string()));

    // Rerun. The same rows are offered again; post 1 is already captured, so it
    // is not re-fetched, and nothing from sequence 000001 is touched.
    let before = std::fs::read(fx.archive.join("post/captures/000001/1.json")).unwrap();
    let out2 = fx.collect(&server.base, &["--no-sweep", "--no-refetch"]);
    assert!(
        out2.status.success(),
        "{}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let after = std::fs::read(fx.archive.join("post/captures/000001/1.json")).unwrap();
    assert_eq!(
        before, after,
        "a rerun must never rewrite an existing packet"
    );
    assert!(
        fx.archive
            .join("changes/captures/000002/page-0001.json")
            .exists(),
        "the rerun's own evidence lands in a fresh sequence"
    );
    assert!(
        !fx.archive.join("post/captures/000002/1.json").exists(),
        "an already-captured post must not be re-fetched by the discovery path"
    );
}

#[test]
fn the_id_sweep_reaches_posts_the_changes_feed_never_mentions() {
    // This is the live finding reproduced: /api/changes omits moderated posts,
    // so ids 2 and 4 are invisible to it. Only integer probing finds them.
    let mut routes = site_routes();
    routes.insert(
        "/api/changes?since=0".into(),
        Canned::json(
            r#"{"next_since":10,"has_more":false,
                "posts":[{"id":1},{"id":3},{"id":5}],"comments":[]}"#,
        ),
    );
    for id in [1u64, 2, 3, 4, 5] {
        routes.insert(format!("/api/post/{id}"), Canned::json(post_body(id)));
    }
    // 6 and beyond do not exist, so the forward probe must stop on its own.
    let server = serve(routes);

    let fx = fixture();
    let out = fx.collect(&server.base, &["--no-refetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let packets = fx.packets();
    for id in [1u64, 2, 3, 4, 5] {
        assert!(
            packets.contains(&format!("post/captures/000001/{id}.json")),
            "post {id} was not captured; the sweep missed a feed-invisible post"
        );
    }
    assert!(
        !packets.contains(&"post/captures/000001/6.json".to_string()),
        "the forward probe invented a post that does not exist"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("gap sweep:           2 recovered"),
        "{stdout}"
    );
}

#[test]
fn an_unparseable_changes_page_holds_the_cursor() {
    // An HTML error page from a proxy must not read as "no new rows".
    let mut routes = site_routes();
    routes.insert(
        "/api/changes?since=0".into(),
        Canned {
            status: 200,
            content_type: "application/json",
            body: "<html>502 Bad Gateway</html>".into(),
        },
    );
    let server = serve(routes);

    let fx = fixture();
    let out = fx.collect(&server.base, &["--no-sweep", "--no-refetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fx.cursor(), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("parsing changes"), "{stdout}");
    // Nothing that failed to parse was stored as though it were understood.
    assert!(!fx
        .packets()
        .contains(&"changes/captures/000001/page-0001.json".to_string()));
}

#[test]
fn an_unexpected_content_type_is_refused_before_it_is_stored() {
    let mut routes = site_routes();
    routes.insert(
        "/".into(),
        Canned {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: "<html>captive portal</html>".into(),
        },
    );
    routes.insert(
        "/api/changes?since=0".into(),
        Canned::json(r#"{"next_since":1,"has_more":false,"posts":[],"comments":[]}"#),
    );
    let server = serve(routes);

    let fx = fixture();
    let out = fx.collect(&server.base, &["--no-sweep", "--no-refetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("unexpected content-type"), "{stdout}");
    assert!(
        !fx.packets()
            .contains(&"site/captures/000001/front-door.txt".to_string()),
        "an HTML interstitial must not be archived as the front-door policy"
    );
}
