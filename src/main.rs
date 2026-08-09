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
mod withheld;

use std::path::{Path, PathBuf};
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
    json      Print one top-level field of a JSON file (glue for scripts/)
    removals  Check a manifest diff's removals against the withholding register
    help      Show this help

JSON OPTIONS:
    --file <path>             JSON document to read
    --field <key>             Top-level key to print
    --len                     Print the array's length instead of its value

REMOVALS OPTIONS:
    --diff <path>             Manifest diff artifact
    --withheld <path>         Withholding register  (default withheld.json)

COLLECT OPTIONS:
    --root <dir>              Archive packet root      (default archive)
    --state <path>            State file               (default state/collector-state.json)
    --base <url>              API base                 (default https://1f916.ai)
    --max-requests <n>        Total request budget     (default 400)
    --min-interval-ms <n>     Politeness delay         (default 600)
    --max-body-bytes <n>      Per-response cap         (default 16777216)
    --max-attempts <n>        Attempts per request     (default 4)
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
        "json" => cmd_json(&args),
        "removals" => cmd_removals(&args),
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

/// Fail unless every record a diff removes is declared in the register.
fn cmd_removals(a: &Args) -> Result<(), String> {
    let diff = a.opt("diff").ok_or("missing required --diff")?;
    let register = a.get_or("withheld", "withheld.json");
    let n = withheld::check_removals(Path::new(diff), Path::new(register))?;
    match n {
        0 => println!("no removals: the packet set only grew"),
        n => println!("{n} declared withholding(s), all registered in {register}"),
    }
    Ok(())
}

/// Read one top-level field out of a JSON document.
///
/// This exists so `scripts/` needs neither `jq` nor a Python interpreter to read
/// a manifest root: the shell glue around a Rust pipeline should not drag in a
/// second language runtime. Strings print unquoted so they can be captured
/// directly into a shell variable.
fn cmd_json(a: &Args) -> Result<(), String> {
    let path = a.opt("file").ok_or("missing required --file")?.to_string();
    let field = a.opt("field").ok_or("missing required --field")?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {path}: {e}"))?;
    let value = doc
        .get(field)
        .ok_or_else(|| format!("{path} has no top-level field {field:?}"))?;

    if a.has("len") {
        let arr = value
            .as_array()
            .ok_or_else(|| format!("{path}.{field} is not an array"))?;
        println!("{}", arr.len());
        return Ok(());
    }
    match value {
        serde_json::Value::String(s) => println!("{s}"),
        serde_json::Value::Null => return Err(format!("{path}.{field} is null")),
        other => println!("{other}"),
    }
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
    // Refuse plaintext: the archive's claim is about bytes received over an
    // authenticated channel, and bytes fetched over http could have been written
    // by anyone on the path. Loopback is the one exception, so the end-to-end
    // tests can drive a scripted local server. There is deliberately no flag to
    // widen this — an opt-out would eventually get set in a real deployment.
    let loopback = base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:");
    if !base.starts_with("https://") && !loopback {
        return Err(format!(
            "--base must be an https:// URL (or http:// on loopback for tests), got {base:?}"
        ));
    }

    let limits = Limits {
        request_budget: a.num_or("max-requests", 400u32)?,
        min_interval: std::time::Duration::from_millis(a.num_or("min-interval-ms", 600u64)?),
        max_body_bytes: a.num_or("max-body-bytes", 16 * 1024 * 1024usize)?,
        max_attempts: a.num_or("max-attempts", 4u32)?.max(1),
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
