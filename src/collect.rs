//! One collection run.
//!
//! The order of operations is the safety property, not an implementation
//! detail:
//!
//! 1. **Site packets first.** `/api/attest` is the sharpest artifact the archive
//!    holds — it is what converts 1f916's own "you cannot check this" statement
//!    into something checkable — so it is captured before anything that could
//!    exhaust the request budget.
//! 2. **Drain `/api/changes`**, following `next_since`, never `now`.
//! 3. **First-capture every newly discovered post.**
//! 4. **Re-fetch posts whose threads changed.** New comments *are* reported by
//!    the feed, so the set of posts with changed thread state in this window is
//!    exactly the set of `post_id`s seen on the comment rows.
//! 5. **Sweep the id gaps.** `/api/changes` omits every post with a non-null
//!    `mod_state`, so a post moderated before the next poll is unreachable
//!    through the feed. Integer id probing is the only way to see it.
//! 6. **Rotate a bounded re-fetch** over the rest of the corpus, so state
//!    transitions the feed cannot report are eventually captured anyway.
//!
//! The `/api/changes` cursor is advanced only if the drain completed *and* every
//! post it discovered was either captured or definitively absent. Everything
//! after step 4 is best-effort and never strands the cursor.

use std::collections::BTreeSet;
use std::path::Path;

use crate::api::{self, ChangesPage, EventsPage};
use crate::http::{Client, Expect, FetchError, Fetched};
use crate::packet::{CaptureRun, PacketMeta, PacketName, Shard};
use crate::state::CollectorState;

/// Run-shaping knobs. All are bounds, never targets.
pub struct Plan {
    pub base: String,
    pub max_pages: u32,
    /// Ids probed from the gap set per run.
    pub max_gap_probes: usize,
    /// Ids probed past the highest known post per run.
    pub max_forward_probes: usize,
    /// Consecutive 404s past the watermark that end the forward probe.
    pub forward_stop_after: usize,
    /// Posts re-fetched from the rotation per run.
    pub max_rotation: usize,
    pub dry_run: bool,
    pub sweep: bool,
    pub refetch: bool,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            base: "https://1f916.ai".to_string(),
            max_pages: 50,
            max_gap_probes: 25,
            max_forward_probes: 20,
            forward_stop_after: 5,
            max_rotation: 25,
            dry_run: false,
            sweep: true,
            refetch: true,
        }
    }
}

/// What a run did, for the summary line and the workflow log.
#[derive(Default)]
pub struct Report {
    pub capture_seq: u64,
    pub site_packets: usize,
    pub changes_pages: usize,
    pub events_pages: usize,
    pub posts_first_captured: usize,
    pub posts_refetched: usize,
    pub gap_found: usize,
    pub gap_absent: usize,
    pub forward_found: usize,
    pub cursor_before: i64,
    pub cursor_after: i64,
    pub cursor_advanced: bool,
    pub requests: u32,
    pub packets_written: usize,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn print(&self) {
        println!("capture sequence {:06}", self.capture_seq);
        println!("  site packets:        {}", self.site_packets);
        println!("  changes pages:       {}", self.changes_pages);
        println!("  events pages:        {}", self.events_pages);
        println!("  posts first-captured:{}", self.posts_first_captured);
        println!("  posts re-fetched:    {}", self.posts_refetched);
        println!(
            "  gap sweep:           {} recovered, {} confirmed absent",
            self.gap_found, self.gap_absent
        );
        println!("  forward probe:       {} new", self.forward_found);
        println!("  packets written:     {}", self.packets_written);
        println!("  requests issued:     {}", self.requests);
        if self.cursor_advanced {
            println!(
                "  cursor:              {} -> {}",
                self.cursor_before, self.cursor_after
            );
        } else {
            println!(
                "  cursor:              held at {} (run incomplete)",
                self.cursor_before
            );
        }
        for w in &self.warnings {
            println!("  warning: {w}");
        }
    }
}

/// Fixed site endpoints, each with its packet name and expected content type.
/// The path is a compile-time constant in every case.
const SITE_ENDPOINTS: &[(&str, &str, Expect)] = &[
    ("/api/attest", "attest.json", Expect::Json),
    ("/", "front-door.txt", Expect::Text),
    ("/api/official", "official.json", Expect::Json),
    ("/api/docket", "docket.json", Expect::Json),
];

/// Execute one run.
pub fn run(
    archive_root: &Path,
    state_path: &Path,
    plan: &Plan,
    client: &mut Client,
) -> Result<Report, String> {
    let mut state = CollectorState::load(state_path)?;
    let mut report = Report {
        cursor_before: state.changes_cursor,
        cursor_after: state.changes_cursor,
        ..Default::default()
    };

    if plan.dry_run {
        println!("dry run: would collect from {}", plan.base);
        println!("  cursor:            {}", state.changes_cursor);
        println!("  captured posts:    {}", state.captured_post_ids.len());
        println!("  known gaps:        {:?}", state.gap_ids());
        println!("  next capture seq:  {}", state.last_capture_seq + 1);
        return Ok(report);
    }

    let mut run =
        CaptureRun::open(archive_root, state.last_capture_seq).map_err(|e| e.to_string())?;
    report.capture_seq = run.seq();

    // ── 1. site packets ──────────────────────────────────────────────────────
    for (path, name, expect) in SITE_ENDPOINTS {
        let url = format!("{}{}", plan.base, path);
        match fetch_and_store(client, &mut run, Shard::Site, name_of(name), &url, *expect) {
            Ok(()) => report.site_packets += 1,
            Err(e) => report.warnings.push(format!("{path}: {e}")),
        }
    }
    let treasury_url = format!("{}/treasury", plan.base);
    match fetch_and_store(
        client,
        &mut run,
        Shard::Treasury,
        PacketName::Fixed("treasury.json"),
        &treasury_url,
        Expect::Json,
    ) {
        Ok(()) => report.site_packets += 1,
        Err(e) => report.warnings.push(format!("/treasury: {e}")),
    }

    // The complete ascending event log, not the moderation-filtered view: the
    // hash chain links events of every kind, so `prev_hash` in a filtered page
    // points at rows the page does not contain and the chain cannot be checked
    // from it.
    match drain_events(client, &mut run, plan) {
        Ok(pages) => report.events_pages = pages,
        Err(e) => report.warnings.push(format!("/api/events: {e}")),
    }

    // ── 2. drain /api/changes ────────────────────────────────────────────────
    let drain = drain_changes(client, &mut run, plan, state.changes_cursor);
    report.changes_pages = drain.pages;
    let mut cursor_safe = drain.complete;
    if let Some(e) = &drain.error {
        report.warnings.push(format!("/api/changes: {e}"));
    }

    // ── 3. first-capture newly discovered posts ──────────────────────────────
    for id in &drain.post_ids {
        if !state.needs_capture(*id) {
            continue;
        }
        match capture_post(client, &mut run, plan, *id) {
            Ok(true) => {
                state.record_present(*id);
                report.posts_first_captured += 1;
            }
            Ok(false) => state.record_absent(*id),
            Err(e) => {
                // A discovered post we failed to fetch must be offered again, so
                // the cursor stays put.
                cursor_safe = false;
                report.warnings.push(format!("post {id}: {e}"));
                break;
            }
        }
    }

    // ── 4. re-fetch posts whose threads changed in this window ───────────────
    if plan.refetch {
        let touched: BTreeSet<u64> = drain
            .comment_post_ids
            .iter()
            .copied()
            .filter(|id| state.captured_post_ids.contains(id))
            .collect();
        for id in touched {
            match capture_post(client, &mut run, plan, id) {
                Ok(true) => report.posts_refetched += 1,
                Ok(false) => state.record_absent(id),
                Err(e) => {
                    report.warnings.push(format!("re-fetch post {id}: {e}"));
                    break;
                }
            }
        }
    }

    // ── 5. gap sweep ─────────────────────────────────────────────────────────
    // These are the posts /api/changes structurally cannot deliver.
    if plan.sweep {
        for id in state.gap_ids().into_iter().take(plan.max_gap_probes) {
            match capture_post(client, &mut run, plan, id) {
                Ok(true) => {
                    state.record_present(id);
                    report.gap_found += 1;
                }
                Ok(false) => {
                    state.record_absent(id);
                    report.gap_absent += 1;
                }
                Err(e) => {
                    report.warnings.push(format!("gap probe {id}: {e}"));
                    break;
                }
            }
        }

        // Forward probe: a post created and moderated between two polls never
        // appears in the feed at all, so the only evidence it existed is its id.
        let mut misses = 0usize;
        let mut probed = 0usize;
        let mut id = state.max_present_post_id + 1;
        while misses < plan.forward_stop_after && probed < plan.max_forward_probes {
            probed += 1;
            match capture_post(client, &mut run, plan, id) {
                Ok(true) => {
                    state.record_present(id);
                    report.forward_found += 1;
                    misses = 0;
                }
                Ok(false) => misses += 1,
                Err(e) => {
                    report.warnings.push(format!("forward probe {id}: {e}"));
                    break;
                }
            }
            id += 1;
        }
    }

    // ── 6. bounded rotation ──────────────────────────────────────────────────
    if plan.refetch {
        for id in state.take_rotation(plan.max_rotation) {
            match capture_post(client, &mut run, plan, id) {
                Ok(true) => report.posts_refetched += 1,
                Ok(false) => state.record_absent(id),
                Err(e) => {
                    report.warnings.push(format!("rotation post {id}: {e}"));
                    break;
                }
            }
        }
    }

    // ── commit ───────────────────────────────────────────────────────────────
    // Every packet above was written with `create_new` + fsync before this point.
    // Only now does the cursor move.
    report.packets_written = run.written().len();
    report.requests = client.spent();
    state.last_capture_seq = run.seq();
    state.last_run_ms = now_ms();
    state.runs += 1;
    if cursor_safe {
        if let Some(next) = drain.next_cursor {
            state.changes_cursor = next;
            report.cursor_after = next;
            report.cursor_advanced = next != report.cursor_before;
        }
    }
    state.save(state_path)?;
    Ok(report)
}

/// Result of draining `/api/changes`.
#[derive(Default)]
struct Drain {
    pages: usize,
    post_ids: Vec<u64>,
    comment_post_ids: BTreeSet<u64>,
    next_cursor: Option<i64>,
    /// True only if the feed reported `has_more: false`.
    complete: bool,
    error: Option<String>,
}

fn drain_changes(client: &mut Client, run: &mut CaptureRun, plan: &Plan, from: i64) -> Drain {
    let mut d = Drain::default();
    let mut seen_posts: BTreeSet<u64> = BTreeSet::new();
    let mut cursor = from;
    for page in 1..=plan.max_pages {
        let url = format!("{}/api/changes?since={}", plan.base, cursor);
        let fetched = match client.get(&url, Expect::Json) {
            Ok(f) => f,
            Err(e) => {
                d.error = Some(e.to_string());
                return d;
            }
        };
        // Parse before storing: an unparseable page must not silently look like
        // "no new rows", and storing it would imply we understood it.
        let parsed: ChangesPage = match api::parse(&fetched.body, "changes") {
            Ok(p) => p,
            Err(e) => {
                d.error = Some(e);
                return d;
            }
        };
        if let Err(e) = store(run, Shard::Changes, PacketName::Page(page), &fetched) {
            d.error = Some(e);
            return d;
        }
        d.pages += 1;

        for p in &parsed.posts {
            if seen_posts.insert(p.id) {
                d.post_ids.push(p.id);
            }
        }
        for c in &parsed.comments {
            d.comment_post_ids.insert(c.post_id);
        }

        let Some(next) = parsed.next_since else {
            // No cursor offered: treat the drain as complete at the old cursor
            // rather than inventing a position.
            d.complete = !parsed.has_more;
            return d;
        };
        // A non-advancing cursor would loop forever against a paged feed.
        if parsed.has_more && next <= cursor {
            d.error = Some(format!(
                "next_since {next} did not advance past {cursor} while has_more was true"
            ));
            d.next_cursor = Some(cursor);
            return d;
        }
        cursor = next;
        d.next_cursor = Some(next);
        if !parsed.has_more {
            d.complete = true;
            return d;
        }
    }
    d.error = Some(format!(
        "page cap {} reached with has_more still true",
        plan.max_pages
    ));
    d
}

fn drain_events(client: &mut Client, run: &mut CaptureRun, plan: &Plan) -> Result<usize, String> {
    let mut pages = 0usize;
    let mut since: i64 = 0;
    for page in 1..=plan.max_pages {
        let url = format!("{}/api/events?since={}", plan.base, since);
        let fetched = client.get(&url, Expect::Json).map_err(|e| e.to_string())?;
        let parsed: EventsPage = api::parse(&fetched.body, "events")?;
        store(run, Shard::Events, PacketName::Page(page), &fetched)?;
        pages += 1;
        let Some(next) = parsed.next_since else {
            return Ok(pages);
        };
        if !parsed.has_more {
            return Ok(pages);
        }
        if next <= since {
            return Err(format!(
                "events next_since {next} did not advance past {since}"
            ));
        }
        since = next;
    }
    Err(format!("events page cap {} reached", plan.max_pages))
}

/// Fetch `/api/post/{id}`. `Ok(false)` means a definitive 404.
fn capture_post(
    client: &mut Client,
    run: &mut CaptureRun,
    plan: &Plan,
    id: u64,
) -> Result<bool, String> {
    let url = format!("{}/api/post/{}", plan.base, id);
    match client.get(&url, Expect::Json) {
        Ok(f) => {
            store(run, Shard::Post, PacketName::NumericId(id), &f)?;
            Ok(true)
        }
        Err(FetchError::NotFound) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

fn fetch_and_store(
    client: &mut Client,
    run: &mut CaptureRun,
    shard: Shard,
    name: PacketName,
    url: &str,
    expect: Expect,
) -> Result<(), String> {
    let f = client.get(url, expect).map_err(|e| e.to_string())?;
    store(run, shard, name, &f)
}

fn store(run: &mut CaptureRun, shard: Shard, name: PacketName, f: &Fetched) -> Result<(), String> {
    run.write(
        shard,
        name,
        &f.body,
        &PacketMeta {
            url: &f.url,
            status: f.status,
            content_type: &f.content_type,
            server_date: f.server_date.as_deref(),
            fetched_at_ms: f.fetched_at_ms,
            elapsed_ms: f.elapsed_ms,
            attempts: f.attempts,
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Turn a `&'static str` from [`SITE_ENDPOINTS`] back into a [`PacketName`].
/// The table is a compile-time constant, so this cannot introduce upstream text.
fn name_of(name: &'static str) -> PacketName {
    PacketName::Fixed(name)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_site_endpoint_path_is_a_constant_without_interpolation() {
        for (path, name, _) in SITE_ENDPOINTS {
            assert!(path.starts_with('/'), "{path}");
            assert!(!path.contains("{"), "{path}");
            assert!(!name.contains('/'), "{name}");
            assert!(!name.contains(".."), "{name}");
        }
        // /api/attest is captured first, deliberately: it is the packet that
        // makes 1f916's self-attestation externally checkable, so a run that
        // exhausts its budget must still have it.
        assert_eq!(SITE_ENDPOINTS[0].0, "/api/attest");
    }

    #[test]
    fn urls_are_built_only_from_the_operator_base_and_integers() {
        let plan = Plan::default();
        assert_eq!(
            format!("{}/api/post/{}", plan.base, 506u64),
            "https://1f916.ai/api/post/506"
        );
        assert_eq!(
            format!("{}/api/changes?since={}", plan.base, -1i64),
            "https://1f916.ai/api/changes?since=-1"
        );
    }

    #[test]
    fn default_plan_bounds_every_unbounded_loop() {
        let p = Plan::default();
        assert!(p.max_pages > 0);
        assert!(p.max_gap_probes > 0);
        assert!(p.max_forward_probes > 0);
        assert!(p.forward_stop_after > 0);
        assert!(p.max_rotation > 0);
    }
}
