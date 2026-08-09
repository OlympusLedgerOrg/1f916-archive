//! Bounded, polite, read-only HTTP.
//!
//! Every response from 1f916.ai is treated as adversarial bytes. This module is
//! the only place they enter the process, and it enforces the limits that make
//! that safe:
//!
//! * a hard byte cap applied to the *decoded* entity body, so a compressed
//!   response cannot expand past the cap,
//! * connect and read timeouts,
//! * a content-type allowlist checked before the body is read,
//! * an identifying `User-Agent` carrying a contact URL,
//! * a minimum interval between requests, a bounded retry budget with full
//!   jitter, and `429` / `Retry-After` handling.
//!
//! The returned [`Fetched`] carries raw bytes plus locally observed metadata.
//! Nothing in here parses, renders, or interprets the body.

use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The content types the collector is willing to store, per endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// `application/json`
    Json,
    /// `text/plain`
    Text,
}

impl Expect {
    fn allows(self, content_type: &str) -> bool {
        // Compare the media type only; parameters such as `; charset=utf-8` are
        // ignored, and matching is ASCII-case-insensitive per RFC 9110.
        let media = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match self {
            Expect::Json => media == "application/json",
            Expect::Text => media == "text/plain",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Expect::Json => "application/json",
            Expect::Text => "text/plain",
        }
    }
}

/// A successfully fetched, bounded response body plus locally observed metadata.
///
/// `body` is the decoded entity body exactly as received. `meta_*` fields are
/// *collector* observations — they are written to a sidecar packet and never
/// merged into the body.
pub struct Fetched {
    pub body: Vec<u8>,
    pub url: String,
    pub status: u16,
    pub content_type: String,
    /// Server `Date` header verbatim, if present. Recorded as a claim, not as
    /// a trusted time source.
    pub server_date: Option<String>,
    /// Collector wall-clock at request start, milliseconds since the Unix epoch.
    pub fetched_at_ms: u64,
    pub elapsed_ms: u64,
    pub attempts: u32,
}

/// A failed fetch, distinguishing "the resource is absent" from everything else
/// so an ID sweep can record absence without aborting the run.
#[derive(Debug)]
pub enum FetchError {
    /// HTTP 404 — the caller decides whether this is expected.
    NotFound,
    /// Any other terminal failure.
    Failed(String),
    /// The run's request budget is exhausted; stop cleanly without advancing
    /// the cursor.
    BudgetExhausted,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NotFound => write!(f, "404 not found"),
            FetchError::Failed(m) => write!(f, "{m}"),
            FetchError::BudgetExhausted => write!(f, "request budget exhausted"),
        }
    }
}

/// Tunables for [`Client`]. Defaults are conservative on purpose: the collector
/// is an indefinite guest of someone else's public infrastructure.
pub struct Limits {
    pub max_body_bytes: usize,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub min_interval: Duration,
    pub max_attempts: u32,
    /// A `Retry-After` longer than this aborts the run instead of sleeping.
    pub max_retry_after: Duration,
    /// Total requests this process may issue.
    pub request_budget: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 16 * 1024 * 1024,
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(45),
            min_interval: Duration::from_millis(600),
            max_attempts: 4,
            max_retry_after: Duration::from_secs(300),
            request_budget: 400,
        }
    }
}

/// Rate-limited HTTP client.
pub struct Client {
    agent: ureq::Agent,
    limits: Limits,
    last_request: Option<Instant>,
    spent: u32,
    rng: u64,
}

/// The `User-Agent` sent with every request. It names the project and carries a
/// contact URL so an operator who wants the collector to stop can find us.
pub const USER_AGENT: &str = concat!(
    "1f916-archive-collector/",
    env!("CARGO_PKG_VERSION"),
    " (read-only verifiable archive; contact: ",
    "https://github.com/OlympusLedgerOrg/1f916-archive",
    ")"
);

impl Client {
    pub fn new(limits: Limits) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(limits.connect_timeout)
            .timeout_read(limits.read_timeout)
            .user_agent(USER_AGENT)
            .redirects(0)
            .build();
        Self {
            agent,
            limits,
            last_request: None,
            spent: 0,
            rng: seed(),
        }
    }

    /// Requests issued so far.
    pub fn spent(&self) -> u32 {
        self.spent
    }

    /// Requests remaining in this process's budget.
    pub fn remaining(&self) -> u32 {
        self.limits.request_budget.saturating_sub(self.spent)
    }

    /// Fetch `url`, enforcing every limit in this module.
    pub fn get(&mut self, url: &str, expect: Expect) -> Result<Fetched, FetchError> {
        if self.remaining() == 0 {
            return Err(FetchError::BudgetExhausted);
        }
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            if self.remaining() == 0 {
                return Err(FetchError::BudgetExhausted);
            }
            self.throttle();
            self.spent += 1;
            let started = Instant::now();
            let fetched_at_ms = now_ms();
            let outcome = self.agent.get(url).call();

            let retry_delay = match outcome {
                Ok(resp) => {
                    let f = self.finish(resp, url, expect, fetched_at_ms, started, attempt);
                    return f;
                }
                Err(ureq::Error::Status(404, _)) => return Err(FetchError::NotFound),
                Err(ureq::Error::Status(429, resp)) => {
                    match retry_after(resp.header("retry-after")) {
                        Some(d) if d > self.limits.max_retry_after => {
                            return Err(FetchError::Failed(format!(
                                "429 with Retry-After {}s exceeds the {}s ceiling; \
                                 stopping without advancing the cursor",
                                d.as_secs(),
                                self.limits.max_retry_after.as_secs()
                            )))
                        }
                        // Honour an explicit Retry-After exactly; otherwise back off.
                        Some(d) => Some(d),
                        None => Some(self.backoff(attempt)),
                    }
                }
                Err(ureq::Error::Status(code, _)) if (500..600).contains(&code) => {
                    Some(self.backoff(attempt))
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(FetchError::Failed(format!("HTTP {code} for {url}")))
                }
                Err(ureq::Error::Transport(t)) => {
                    if attempt >= self.limits.max_attempts {
                        return Err(FetchError::Failed(format!(
                            "transport error for {url}: {t}"
                        )));
                    }
                    Some(self.backoff(attempt))
                }
            };

            if attempt >= self.limits.max_attempts {
                return Err(FetchError::Failed(format!(
                    "giving up on {url} after {attempt} attempts"
                )));
            }
            if let Some(d) = retry_delay {
                std::thread::sleep(d);
            }
        }
    }

    /// Validate the response envelope, then read a bounded body.
    fn finish(
        &mut self,
        resp: ureq::Response,
        url: &str,
        expect: Expect,
        fetched_at_ms: u64,
        started: Instant,
        attempts: u32,
    ) -> Result<Fetched, FetchError> {
        let status = resp.status();
        let content_type = resp.header("content-type").unwrap_or_default().to_string();
        let server_date = resp.header("date").map(str::to_string);

        // Reject the type *before* reading the body, so an unexpected payload is
        // never pulled into memory or written to disk.
        if !expect.allows(&content_type) {
            return Err(FetchError::Failed(format!(
                "unexpected content-type {content_type:?} for {url} (expected {})",
                expect.describe()
            )));
        }
        // Trust the declared length only as an early reject; the real cap is the
        // read below, which also bounds a mis-declared or compressed body.
        if let Some(declared) = resp
            .header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            if declared > self.limits.max_body_bytes as u64 {
                return Err(FetchError::Failed(format!(
                    "declared Content-Length {declared} exceeds the {}-byte cap for {url}",
                    self.limits.max_body_bytes
                )));
            }
        }

        // Read one byte past the cap so an over-size body is detected rather than
        // silently truncated into the archive.
        let cap = self.limits.max_body_bytes;
        let mut body = Vec::new();
        resp.into_reader()
            .take(cap as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| FetchError::Failed(format!("reading body of {url}: {e}")))?;
        if body.len() > cap {
            return Err(FetchError::Failed(format!(
                "body of {url} exceeds the {cap}-byte cap"
            )));
        }

        Ok(Fetched {
            body,
            url: url.to_string(),
            status,
            content_type,
            server_date,
            fetched_at_ms,
            elapsed_ms: started.elapsed().as_millis() as u64,
            attempts,
        })
    }

    /// Sleep so that consecutive requests are at least `min_interval` apart.
    fn throttle(&mut self) {
        if let Some(last) = self.last_request {
            let since = last.elapsed();
            if since < self.limits.min_interval {
                std::thread::sleep(self.limits.min_interval - since);
            }
        }
        self.last_request = Some(Instant::now());
    }

    /// Exponential backoff with full jitter, capped at 30s.
    fn backoff(&mut self, attempt: u32) -> Duration {
        let base_ms = 1000u64.saturating_mul(1 << attempt.min(5));
        let capped = base_ms.min(30_000);
        Duration::from_millis(self.next_rand() % (capped + 1))
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*: enough for jitter, and avoids a dependency on this path.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Parse a `Retry-After` header. Only the delta-seconds form is honoured; an
/// HTTP-date falls through to normal backoff rather than being mis-parsed.
fn retry_after(header: Option<&str>) -> Option<Duration> {
    let secs: u64 = header?.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    // Never seed the generator with zero: xorshift is stuck at zero forever.
    nanos | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_expectation_ignores_parameters_and_case() {
        assert!(Expect::Json.allows("application/json"));
        assert!(Expect::Json.allows("application/json; charset=utf-8"));
        assert!(Expect::Json.allows("APPLICATION/JSON"));
        assert!(!Expect::Json.allows("text/html"));
        assert!(!Expect::Json.allows("application/json+ld"));
        // A prefix match would wrongly accept this; the media type must be exact.
        assert!(!Expect::Json.allows("application/jsonish"));
    }

    #[test]
    fn text_expectation_rejects_html() {
        assert!(Expect::Text.allows("text/plain; charset=utf-8"));
        // An error page or an injected redirect interstitial must not be stored
        // as if it were the front-door policy text.
        assert!(!Expect::Text.allows("text/html; charset=utf-8"));
        assert!(!Expect::Text.allows(""));
    }

    #[test]
    fn retry_after_accepts_seconds_and_ignores_http_date() {
        assert_eq!(retry_after(Some("120")), Some(Duration::from_secs(120)));
        assert_eq!(retry_after(Some(" 5 ")), Some(Duration::from_secs(5)));
        assert_eq!(retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        assert_eq!(retry_after(None), None);
    }

    #[test]
    fn user_agent_identifies_the_project_and_a_contact() {
        assert!(USER_AGENT.contains("1f916-archive-collector"));
        assert!(USER_AGENT.contains("https://"));
    }

    #[test]
    fn backoff_is_bounded_and_jittered() {
        let mut c = Client::new(Limits::default());
        for attempt in 1..=8 {
            for _ in 0..32 {
                assert!(c.backoff(attempt) <= Duration::from_millis(30_000));
            }
        }
        // Full jitter must actually vary, or a fleet of retries stays in lockstep.
        let samples: Vec<_> = (0..16).map(|_| c.backoff(3)).collect();
        assert!(samples.iter().any(|d| *d != samples[0]));
    }

    #[test]
    fn budget_is_enforced_before_any_request() {
        let mut c = Client::new(Limits {
            request_budget: 0,
            ..Limits::default()
        });
        // No network is touched: the budget check precedes the connection.
        match c.get("https://example.invalid/", Expect::Json) {
            Err(FetchError::BudgetExhausted) => {}
            other => panic!(
                "expected BudgetExhausted, got {other:?}",
                other = match other {
                    Ok(_) => "Ok".to_string(),
                    Err(e) => e.to_string(),
                }
            ),
        }
    }
}
