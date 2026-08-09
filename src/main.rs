//! `f916-collect` — read-only collector for a verifiable 1f916.ai archive.
//!
//! The whole program is one pipeline, and it is deliberately short:
//!
//! ```text
//! HTTPS fetch (bounded) -> raw byte write (immutable) -> [olympus build]
//! ```
//!
//! Captured bytes are never rendered, executed, templated, shell-interpolated,
//! summarised, classified, or sent to a language model. The only values taken
//! *out* of a response are integers (see `api.rs`), and the only inputs to a
//! filesystem path are those integers and locally assigned capture sequence
//! numbers (see `packet.rs`).
//!
//! Manifest building, diffing and anchoring are not this binary's job: they are
//! the pinned Olympus `olympus` CLI plus `cosign`, driven by `scripts/` and the
//! scheduled workflow.

mod api;
mod args;
mod collect;
mod http;
mod packet;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;

use args::Args;
use collect::Plan;
use http::{Client, Limits};

const USAGE: &str = "\
f916-collect — read-only collector for a verifiable 1f916.ai archive

USAGE:
    f916-collect <command> [options]

COMMANDS:
    collect   Run one capture: site packets, changes drain, post fetches, sweeps
    state     Print committed collector state and exit
    help      Show this help

COLLECT OPTIONS:
    --root <dir>              Archive packet root      (default archive)
    --state <path>            State file               (default state/collector-state.json)
    --base <url>              API base                 (default https://1f916.ai)
    --max-requests <n>        Total request budget     (default 400)
    --min-interval-ms <n>     Politeness delay         (default 600)
    --max-body-bytes <n>      Per-response cap         (default 16777216)
    --max-pages <n>           Drain page cap           (default 50)
    --max-gap-probes <n>      Gap-sweep probes/run     (default 25)
    --max-forward-probes <n>  Forward probes/run       (default 20)
    --max-rotation <n>        Rotation re-fetches/run  (default 25)
    --initial                 Re-drain from since=0 into a new capture sequence
    --no-sweep                Skip id-gap and forward probing
    --no-refetch              Skip thread-change and rotation re-fetches
    --dry-run                 Print the plan and committed state, fetch nothing
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = argv.first().cloned() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let args = Args::parse(argv.into_iter().skip(1));

    let result = match command.as_str() {
        "collect" => cmd_collect(&args),
        "state" => cmd_state(&args),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn state_path(a: &Args) -> PathBuf {
    PathBuf::from(a.get_or("state", "state/collector-state.json"))
}

fn cmd_state(a: &Args) -> Result<(), String> {
    let s = state::CollectorState::load(&state_path(a))?;
    println!("schema:              {}", s.schema);
    println!("changes_cursor:      {}", s.changes_cursor);
    println!("last_capture_seq:    {}", s.last_capture_seq);
    println!("captured posts:      {}", s.captured_post_ids.len());
    println!("known absent:        {}", s.absent_post_ids.len());
    println!("max present post id: {}", s.max_present_post_id);
    println!("outstanding gaps:    {:?}", s.gap_ids());
    println!("runs completed:      {}", s.runs);
    println!("last run (unix ms):  {}", s.last_run_ms);
    Ok(())
}

fn cmd_collect(a: &Args) -> Result<(), String> {
    if a.has("help") {
        print!("{USAGE}");
        return Ok(());
    }
    let root = PathBuf::from(a.get_or("root", "archive"));
    let state_file = state_path(a);

    let base = a.get_or("base", "https://1f916.ai").trim_end_matches('/');
    // Refuse anything but HTTPS: the archive's claim is about bytes received
    // over an authenticated channel.
    if !base.starts_with("https://") {
        return Err(format!("--base must be an https:// URL, got {base:?}"));
    }

    let limits = Limits {
        request_budget: a.num_or("max-requests", 400u32)?,
        min_interval: std::time::Duration::from_millis(a.num_or("min-interval-ms", 600u64)?),
        max_body_bytes: a.num_or("max-body-bytes", 16 * 1024 * 1024usize)?,
        ..Limits::default()
    };
    let plan = Plan {
        base: base.to_string(),
        max_pages: a.num_or("max-pages", 50u32)?,
        max_gap_probes: a.num_or("max-gap-probes", 25usize)?,
        max_forward_probes: a.num_or("max-forward-probes", 20usize)?,
        max_rotation: a.num_or("max-rotation", 25usize)?,
        dry_run: a.has("dry-run"),
        sweep: !a.has("no-sweep"),
        refetch: !a.has("no-refetch"),
        ..Plan::default()
    };

    if a.has("initial") && !plan.dry_run {
        // Reset only the cursor. Captured-id bookkeeping is a monotone record of
        // what exists on disk and must survive, or the re-drain would try to
        // first-capture posts whose packets are already written and fail on the
        // overwrite guard.
        let mut s = state::CollectorState::load(&state_file)?;
        s.changes_cursor = 0;
        s.save(&state_file)?;
        println!("--initial: cursor reset to 0 (captured-id record retained)");
    }

    let mut client = Client::new(limits);
    let report = collect::run(&root, &state_file, &plan, &mut client)?;
    report.print();
    Ok(())
}
