# Write-surface reconnaissance: method and open questions

> **Status: measured 2026-08-10**, by
> [run 31404565832](https://github.com/OlympusLedgerOrg/1f916-archive/actions/runs/31404565832)
> and confirmed identical 74 minutes later by
> [run 31411026576](https://github.com/OlympusLedgerOrg/1f916-archive/actions/runs/31411026576)
> — 51 observations over 27 paths each, no transport errors. The API is someone
> else's and may change without notice.
>
> **Headline: the instrument did not work on this host.** `OPTIONS` returned `200`
> with an identical blanket CORS list on all 24 paths it was sent to, and not one
> of the 27 probed paths returned an `Allow` header in any response. The write
> surface is therefore **not mappable by `OPTIONS`**, which was the whole premise,
> and most questions below close as unanswered rather than answered. That is a
> real result, and it changes the plan.
>
> (The three control paths were probed with `GET` only — they exist to prove the
> run reached a live host, not to measure method sets. So the `OPTIONS` finding
> covers 24 paths; the `Allow` finding covers all 27.)

**Method:** read-only. `GET` and `OPTIONS` only — both `safe` per RFC 9110
§9.2.1, both creating no upstream state. Run by
[`scripts/recon-write-surface.sh`](../scripts/recon-write-surface.sh), which has
no code path that can issue any other method. **No writes to 1f916 of any kind**,
which keeps the claim in `api-semantics.md` true for the duration of the recon.

---

## Why the write surface needs measuring at all

The archive is considering a **canary**: a small number of posts authored by this
project, whose content is anchored in the transparency log *before* it is
published upstream. The point is narrow. Every other record in this archive is
content the archive cannot vouch for — `README.md` says so in the third row of
its own table, and that row is not negotiable. A canary is the one class of
record where the archive knows the ground truth, because it wrote it. That makes
it a calibration record: it measures whether the pipeline — upstream storage,
upstream hash chain, the collector, the manifest — carries a known input through
faithfully.

It is not a truth claim about 1f916's content, and it does not become one. It is
a test vector.

Building it requires facts the archive does not have. This recon collects them
before any credential exists, because the decision about whether to hold a
credential at all depends on the answers.

## What a read-only recon of a *write* surface can establish

`OPTIONS` is what makes this possible. It reports which methods an endpoint
implements without attempting them, so the shape of the write surface can be
mapped without a single write. Combined with the status and challenge returned by
an unauthenticated `GET`, it distinguishes the cases that matter:

| observation | reading |
|---|---|
| `404` on both methods | no such endpoint — **weak evidence**, see below |
| `405` with an `Allow` listing `POST` | the endpoint exists and takes writes |
| `401`/`403` with a `WWW-Authenticate` scheme | exists, and names the credential type it wants |
| `200` on `GET` | a readable endpoint; its key shape says what it models |
| `Access-Control-Allow-Methods` listing `POST` | **nothing about the write surface** — see below |

Only `Allow` answers the question. `Allow` is the origin server stating which
methods *this resource* implements (RFC 9110 §10.2.1).
`Access-Control-Allow-Methods` is a CORS policy addressed to browsers, and
middleware routinely emits a blanket `GET, POST, PUT, DELETE, OPTIONS` on every
route regardless of what the route actually does. The probe records the two in
separate fields (`allow`, `cors_allow_methods`) and the write-surface conclusion
is drawn from `allow` alone. Conflating them would manufacture a write surface on
every path probed — a false positive in the one direction that would matter, since
it is the direction that argues for acquiring a credential.

### What actually happened: the `Allow` column came back empty everywhere

Not one of the 27 probed paths returned an `Allow` header — not the ones that
answered `200`, not the ones that answered `404`. All 24 `OPTIONS` requests
returned `200` with `Access-Control-Allow-Methods: GET, OPTIONS, POST`,
identically, including on paths whose `GET` returned `404`: `/api/apply`,
`/api/join`, `/api/vote`, `/api/flag`, `/api/docs`. A CORS preflight handler is
answering ahead of routing, so `OPTIONS` carries no per-resource information
whatsoever.

(Measurement language on purpose: a `404` is an observed response, not a proof of
absence — this document says so two sections down, and should not quietly assume
otherwise here.)

Two consequences, and the second is the one that matters.

**The table above collapses.** Its second and fifth rows are the only ones that
could have distinguished "takes writes" from "does not exist", and neither ever
fired. On this host `OPTIONS` cannot tell those apart — it answers `200 … POST`
for both.

**The CORS separation is what stopped this becoming a fabricated result.** Had
the probe merged `Allow` with `Access-Control-Allow-Methods`, as it did before
review, this report would now state that `POST` is available on all 27 paths,
including `/api/apply` and `/api/flag`, which do not exist. It would have
manufactured a complete write surface out of one middleware default, and every
downstream decision — including "acquire a credential" — would have rested on it.
The distinction was worth drawing.

**Where it stops.** A `404` is not proof of absence: an endpoint can be
undocumented, differently named, versioned under a prefix this probe does not
guess, or gated behind a session the probe does not hold. And no safe method can
reveal what a *successful* write does — the accepted body shape, the validation
rules, the response. Those are knowable only by writing, and this recon does not
write. Anything in that category stays an open question and gets answered, if at
all, by a deliberate first write under the design this recon informs.

## The safety split in the output

The probe writes two things, and the separation is the reason the recon can be
written up honestly:

- `structure.json` — status codes, media types, `Allow` tokens, CORS method
  policy, auth scheme tokens, body lengths, and JSON **key paths with every value
  stripped**. Machine-readable, safe to quote and commit: every field lifted out
  of a response is validated against a bounded pattern and dropped rather than
  reproduced if it does not conform.
- `prose/*.body` — raw bodies, verbatim, upstream-controlled.

`README.md` forbids feeding captured content to a language model. That rule binds
whoever writes this report, including an AI assistant helping to write it. So:
every assertion in the results sections must be traceable to `structure.json`.
Anything that required reading `prose/` is a **human reading**, marked as such
and attributed to a person. The existing `api-semantics.md` already meets this
bar without announcing it — note that it describes moderated bodies by *length*
("71-byte placeholder") rather than quoting them.

One residual: JSON key names and header tokens are upstream-controlled text, and
they do cross into `structure.json`. They are filtered to a conservative
character class and length; a key that does not conform is counted, never
reproduced. This is the same exposure `api-semantics.md` §2 already accepts when
it lists the fields of a changes row.

---

## Everything that answered

`OPTIONS` is omitted below: it returned `200` with the same CORS list on all 27
paths and distinguishes nothing (see above). `GET` is the entire signal.

| path | `GET` | media type | reading |
|---|---:|---|---|
| `/api/attest` | 200 | `application/json` | control — live, as expected |
| `/api/official` | 200 | `application/json` | control — live |
| `/api/docket` | 200 | `application/json` | control — live |
| `/` | 200 | `text/plain` | front door, addressed to agents |
| `/robots.txt` | 200 | `text/plain` | present |
| `/.well-known/security.txt` | 200 | `text/plain` | present |
| **`/api/citizens`** | **200** | `application/json` | **citizenship is a real, publicly readable concept** |
| **`/api/me`** | **401** | `application/json` | **exists and is identity-gated** |

### Response shapes — key paths only, values stripped

Recovered from [run 31411026576](https://github.com/OlympusLedgerOrg/1f916-archive/actions/runs/31411026576)
(2026-08-10 16:50), a second dispatch whose observations were **identical** to the
first 74 minutes earlier. Reproducibility is itself a small finding: this is
steady-state behaviour, not a transient.

| response | body | keys |
|---|---:|---|
| `/api/citizens` `200` | 59 730 B | `citizens[].handle`, `citizens[].model`, `citizens[].karma`, `citizens[].votes_cast`, `citizens[].created_at`, `count`, `note`, `page_size`, `returned`, `total`, `now`, `now_utc` |
| `/api/me` `401` | 126 B | `error`, `now`, `now_utc` |
| every `404` | 133 B | `error`, **`hint`**, `now`, `now_utc` |
| `/.well-known/security.txt` | 809 B | *(text/plain — not parsed)* |

Zero non-conforming keys anywhere, so nothing was dropped by the character-class
filter.

**Two things here matter more than the table above.**

**Every `404` carries a `hint`.** The API documents itself on failure. The body is
133 bytes on all nineteen `404`s — byte-identical in length, so this is a constant
message rather than a per-path suggestion, but it is still the API telling a caller
something about how to use it correctly. Nobody has read it. It is prose, so
reading it is a human job (§4), and it is now the **cheapest single unread thing in
this whole investigation** — 133 bytes that the server offers to anyone who asks
for a wrong path.

**The `401` deliberately withholds that hint.** `/api/me` returns `error`, `now`,
`now_utc` and *no* `hint` key, while a routing failure gets one. The API is more
forthcoming about wrong paths than about missing credentials. That asymmetry looks
deliberate, and it is consistent with the other null result: no
`WWW-Authenticate` header either. Whatever the enrolment story is, the API is not
volunteering it to unauthenticated callers.

### What a citizen is, structurally

`handle`, `model`, `karma`, `votes_cast`, `created_at`. So citizenship carries a
declared model — matching the `author_model` field `api-semantics.md` §2 already
records on posts — plus reputation and activity counters, and a creation
timestamp. Citizens are created at a point in time, by some process, and the
roster is paginated (`count`, `page_size`, `returned`, `total`) and public.

`created_at` is the interesting one for a canary: it means the roster carries the
same kind of timestamp the archive already knows how to reason about, and a new
citizen would be visible in it.

Everything else returned `404` on `GET`: `/llms.txt`, `/openapi.json`, `/api`,
`/api/docs`, `/api/openapi.json`, `/api/register`, `/api/signup`, `/api/join`,
`/api/apply`, `/api/citizen`, `/api/session`, `/api/auth`, `/api/token`,
`/api/post`, `/api/posts`, `/api/comment`, `/api/comments`, `/api/vote`,
`/api/flag`.

(`/api/post` returning `404` is expected and reassuring: the collector reads
`/api/post/{id}`, so the collection-level path having no handler is consistent
with the read surface already measured in `api-semantics.md`.)

## The questions

### 1. Does an enrolment path exist, and what does it demand?

**Not found, and not disproved.** All eight guessed enrolment names — `register`,
`signup`, `join`, `apply`, `citizen`, `session`, `auth`, `token` — returned `404`
on `GET`, and `OPTIONS` was uninformative on all of them. No API documentation
exists at any guessed location either (`/openapi.json`, `/api`, `/api/docs`,
`/api/openapi.json` all `404`), so there is no machine-readable index to correct
the guesses from.

Two positive findings survive, and they are the useful part:

- **`/api/citizens` returns `200` JSON.** Citizenship is not a metaphor in the
  posts; it is a modelled entity with a publicly readable roster.
- **`/api/me` returns `401` JSON.** There is a per-identity endpoint, and it
  distinguishes an authenticated caller from an anonymous one.

What those two do **not** establish is that an enrolment *mechanism* is exposed at
all. Citizens exist; nothing observed says how one comes to exist. They could be
provisioned by the operator, invited out of band, seeded, or created by an admin
path with no public route. "A roster is readable" and "anyone may join" are
different claims, and only the first was measured.

That distinction is not pedantic — it changes the next step. If citizenship is
operator-granted rather than self-serve, then asking the operator is not merely
the polite route to the answer, it is the only route, and no amount of path
guessing substitutes for it.

What can be said: the probe did not find an enrolment door, and — given `OPTIONS`
is inert here — the only read-only observation that could still find one is a
`GET` on a correctly guessed name.

**Except that the server may simply tell us.** Every `404` body carries a `hint`
key. That is the API offering guidance to a caller who asked for the wrong path,
and it costs nothing to read. It should be read before anyone considers guessing
further names, because guessing is the expensive, impolite option and this is the
cheap, intended one.

### 2. What credential does citizenship issue?

**Unanswered, and not answerable this way.** `/api/me` returned `401` with **no
`WWW-Authenticate` header at all** — the `auth` column is empty for every row in
the run. A `401` that does not name a scheme tells you a credential is required
and nothing about its type. Bearer token, cookie, signed assertion: all remain
open.

The body shape sharpens this rather than resolving it. The `401` returns exactly
`error`, `now`, `now_utc` — 126 bytes, and **no `hint`**, where every `404` has
one. The API helps with wrong paths and declines to help with missing
credentials. Two independent silences pointing the same way (no scheme header, no
hint) read as a choice rather than an oversight, which is itself worth knowing: the
credential story is unlikely to be discoverable by poking, and asking is the
route.

This is the question the whole custody design depends on, so its remaining open
is the single biggest gap in this recon.

### 3. What is the post-creation endpoint?

**Unanswered, and not answerable this way.** `/api/post` and `/api/posts` both
`404` on `GET`, and `OPTIONS` claims `POST` on them exactly as loudly as it does
on `/api/flag` and `/api/apply`. There is no read-only observation on this host
that separates "a write endpoint I did not find" from "no write endpoint".

### 4. Is automated participation actually permitted?

**Still open, and now the load-bearing question.** The recon located the
documents but cannot read them: three exist and one does not.

| document | status |
|---|---|
| `/` | `200`, `text/plain` |
| `/robots.txt` | `200`, `text/plain` |
| `/.well-known/security.txt` | `200`, `text/plain` |
| `/llms.txt` | `404` — absent |

**This requires a human reading, and the report will name who did it.** The rule
in `README.md` forbids feeding captured content to a language model, so the
assistant that wrote this document has not read any of those three files and must
not. That the front door is `text/plain` and the site is populated by agents makes
it more likely, not less, that its contents are addressed to an LLM — which is
exactly the case the rule exists for.

With §1–§3 closed as unanswerable, this is now the question that decides whether
the canary proceeds at all. If the answer is no or unclear, the canary does not
happen, and that is a complete and acceptable result of this recon.

### 5. What rate and quota apply to writes?

**Nothing observed.** No `429` and no `Retry-After` across 51 requests at
600 ms spacing, so the read-side limits were never approached. Write limits are
undocumented at any endpoint this probe could reach, and would in any case be
published in the same prose that §4 depends on.

### 6. Does a citizen's own post flow through the ordinary read path?

**Unanswerable until a citizen post exists**, as expected — this one was never
going to fall to a probe. The canary is worthless if capturing it needs a special
case: it has to be discovered by the unmodified collector, through
`/api/changes` and `/api/post/{id}`, exactly like any other post.
`api-semantics.md` §5 says posts with a non-null `mod_state` never appear in the
feed; nothing yet says whether an ordinary post by an ordinary citizen behaves
normally, because the archive has never had one to watch.

One adjacent finding is worth carrying forward: `/api/citizens` is readable
without credentials. If a canary ever exists, its author becomes a row in a public
roster — the archive stops being only an observer of that endpoint and becomes an
entry in it. That is a posture change, not a technical obstacle, but it should be
a decision rather than a side effect.

### 7. **Does upstream serve back the exact bytes it was given?**

This is the question that decides the canary's design, and it is worth stating
plainly because it is easy to miss.

Pre-committing `H(content)` before publishing only works if 1f916 stores and
serves that content byte-for-byte. If it trims whitespace, normalises Unicode,
rewrites links, renders Markdown, or re-serialises JSON with different key order,
then `H(submitted) ≠ H(served)`, and a pre-commitment to the submitted bytes
proves nothing about the served ones — it would fail against an *honest* upstream,
which is the worst possible failure mode for a calibration record.

Two designs follow, and the recon picks between them:

- **Byte-exact pre-commitment.** Anchor `H(content)`, publish, capture, compare.
  Strongest claim: the served bytes are the anchored bytes. Fragile — any
  normalisation upstream breaks it, and upstream is free to change that silently.
- **Nonce pre-commitment.** Anchor a high-entropy token, embed it in the content,
  publish, then verify the token survives in the captured packet. Robust to
  normalisation. Weaker claim, but still the one that matters: *this token existed
  upstream no earlier than the anchor that predates it*, and no rewrite can
  retroactively fabricate the ordering.

**Measured: no evidence either way, and none obtainable read-only.** Round-tripping
can only be tested by submitting bytes and reading them back, which is a write.
So the nonce design stands as the default — not as a preference, but because it is
the only one of the two that can be adopted without first assuming the answer.

---

## Where this leaves the canary

Of seven questions, the recon closed **none** affirmatively, and that is the
finding rather than a failure of the run. The probe worked exactly as designed;
the host simply does not expose the signal the design depended on.

What was established:

- Citizenship is real and modelled (`/api/citizens`, `200` JSON), and identity is
  enforced somewhere (`/api/me`, `401`).
- The enrolment and write endpoints are not at any of the 14 names guessed, and
  no machine-readable API description exists to correct the guesses from.
- `OPTIONS` is inert on this host, so **no further `OPTIONS` probing will help** —
  widening the path list would only produce more `200 … POST` answers that mean
  nothing. `GET` on a correctly guessed name could still find a route, so this is
  not a capability limit; **declining to enumerate paths is a choice.** Guessing
  at scale against someone else's service is scanning, whatever it is called, and
  this archive does not have standing to do that to a host it is a guest on. A
  second recon pass of the same kind is not worth running.

Three ways forward, in the order they should be considered:

1. **Read the four unread documents** (§4). Three `text/plain` files — `/`,
   `/robots.txt`, `/.well-known/security.txt` (809 B, so it has real content) —
   plus the 133-byte `hint` the server returns on any `404`. A person has read
   none of them. This is the cheapest remaining step by a wide margin, and it can
   settle the whole question in either direction, including by answering "no",
   which ends the matter.
2. **Ask.** `/.well-known/security.txt` exists, which conventionally carries a
   contact. The archive already identifies itself in every `User-Agent` and calls
   itself "an indefinite guest of someone else's public infrastructure"; asking
   the operator whether an archival canary is welcome is more in keeping with that
   posture than probing for an unlocked door.
3. **Only then**, a deliberate first write under a design this recon can no longer
   inform as much as hoped.

What should **not** happen next is escalating to unauthenticated `POST` attempts
to discover the write surface. That would be probing someone else's service for
undocumented mutation endpoints without asking — it abandons the guest posture,
and it falsifies `api-semantics.md`'s no-writes claim to answer a question that
§4 might answer for free.

---

## What the answers decide

Two of this repository's stated properties change if a canary is built, and
neither should change quietly:

1. **`docs/api-semantics.md`: "No writes to 1f916 of any kind."** Becomes false.
   It gets rewritten to scope the claim to the collector, with the canary's write
   path named explicitly.
2. **`README.md`: "There is no long-lived key anywhere in this project."** Likely
   becomes false — but note this is *not* established. §2 is unresolved, so the
   credential's lifetime is unknown; `/api/me` returned `401` without naming a
   scheme, and a `401` says nothing about how long a credential lives. Narrow the
   claim to the *anchoring* path only once the credential is actually known.

The constraint that follows does not depend on that lifetime at all, which is why
it can be stated now: **whatever credential can post must never be reachable from
the signing workflow.** Today, compromising `capture.yml` gets an attacker a
short-lived Fulcio certificate bound to a workflow identity. If the same workflow
could also post as the archive, one compromise yields both the archive's voice
and its signature, and a forged canary would carry a real anchor. That holds for
a bearer token, a cookie, a refresh secret, or anything else — the exposure is
"can post", not "is long-lived". Separate
workflow, separate secret, separate trigger — and the canary is manual, never on
the hourly schedule.

## Running it

The probe needs network access to `1f916.ai`, which the archive's own CI has and
a sandboxed development environment generally does not. Either dispatch
[`.github/workflows/recon.yml`](../.github/workflows/recon.yml) — manual trigger
only, `contents: read`, no `id-token`, and it commits nothing — or run it locally:

```bash
scripts/recon-write-surface.sh recon-out
# GET only: OPTIONS answers 200 with the same CORS list on every path it is sent
# to here and distinguishes nothing, so including it just pads the output.
#
# `has("status")` is load-bearing: a transport failure is recorded as
# {path, method, transport_error} with no status field, and `null != 404` is true
# in jq — without the guard a failed request prints as a blank-status row and
# reads like a successful observation.
jq -r '.observations[]
       | select(.method == "GET" and has("status") and .status != 404 and .status != 0)
       | [.path, .status, (.media_type // "-"), (.allow // [] | join("+") | if . == "" then "-" else . end), (.auth_scheme // "-")]
       | @tsv' recon-out/structure.json
```

51 requests as currently configured — 3 control `GET`s, then `GET` and `OPTIONS`
for each of 24 candidate paths — spaced 600 ms apart as the collector spaces its
own, so about 35 seconds of wall clock. The run
aborts before probing anything unknown if the known-live control endpoints do not
respond, so a report of uniform `404`s from a host that is merely down cannot be
mistaken for a host with no write surface.

`recon-out/` is gitignored. The structural report may be committed deliberately
once read; the raw bodies beside it are not this repository's to redistribute.
