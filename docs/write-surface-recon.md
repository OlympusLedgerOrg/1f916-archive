# Write-surface reconnaissance: method and open questions

> **Status: not yet measured.** This document is the instrument and the question
> list, not the findings. Every results table below is empty on purpose. Nothing
> here should be cited as a fact about 1f916 until a dated run has filled them
> in, the same way [`api-semantics.md`](api-semantics.md) was filled in on
> 2026-08-09.

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

- `structure.json` — status codes, media types, `Allow` tokens, auth scheme
  tokens, body lengths, and JSON **key paths with every value stripped**.
  Machine-readable, safe to quote and commit.
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

## The questions

### 1. Does an enrolment path exist, and what does it demand?

| path | `GET` | `OPTIONS` `Allow` | challenge | shape |
|---|---|---|---|---|
| *(pending)* | | | | |

### 2. What credential does citizenship issue?

A bearer token, a signed assertion, a session cookie, something else. This
decides the whole custody design, so it is the question the recon most needs to
answer structurally rather than by assumption.

### 3. What is the post-creation endpoint?

Existence and method set only. The accepted body shape is out of scope for a
read-only pass (see *Where it stops*).

### 4. Is automated participation actually permitted?

`/`, `/robots.txt`, `/llms.txt`, `/.well-known/security.txt`. The archive reads
under a courteous-guest posture it has kept deliberately; writing is a larger
imposition and needs an affirmative answer, not the absence of a prohibition.
**This is a human reading of `prose/`, not a structural finding.** If the answer
is no, or unclear, the canary does not happen — that outcome is a complete and
acceptable result of this recon.

### 5. What rate and quota apply to writes?

The collector already honours `429` and `Retry-After` on reads. A canary should
be rare by design — single digits, ever — but the published limits should be
recorded rather than guessed at.

### 6. Does a citizen's own post flow through the ordinary read path?

The canary is worthless if capturing it needs a special case. It has to be
discovered by the unmodified collector, through `/api/changes` and
`/api/post/{id}`, exactly like any other post. `api-semantics.md` §5 says posts
with a non-null `mod_state` never appear in the feed; nothing yet says whether an
ordinary post by an ordinary citizen behaves normally, because the archive has
never had one to watch.

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

Absent evidence of exact-byte round-tripping, the nonce design is the honest
default — it degrades to a true statement instead of a false negative.

---

## What the answers decide

Two of this repository's stated properties change if a canary is built, and
neither should change quietly:

1. **`docs/api-semantics.md`: "No writes to 1f916 of any kind."** Becomes false.
   It gets rewritten to scope the claim to the collector, with the canary's write
   path named explicitly.
2. **`README.md`: "There is no long-lived key anywhere in this project."** A
   citizen credential is a long-lived secret. The claim gets narrowed to the
   *anchoring* path, where it is load-bearing and must stay true.

That second one carries a hard constraint on the design, and the recon exists
partly to price it: **the posting credential must never be reachable from the
signing workflow.** Today, compromising `capture.yml` gets an attacker a
short-lived Fulcio certificate bound to a workflow identity. If the same workflow
could also post as the archive, one compromise yields both the archive's voice
and its signature, and a forged canary would carry a real anchor. Separate
workflow, separate secret, separate trigger — and the canary is manual, never on
the hourly schedule.

## Running it

The probe needs network access to `1f916.ai`, which the archive's own CI has and
a sandboxed development environment generally does not. Either dispatch
[`.github/workflows/recon.yml`](../.github/workflows/recon.yml) — manual trigger
only, `contents: read`, no `id-token`, and it commits nothing — or run it locally:

```bash
scripts/recon-write-surface.sh recon-out
jq -r '.observations[]
       | select(.status != 404)
       | [.method, .path, .status, (.allow // [] | join("+")), (.auth_scheme // "-")]
       | @tsv' recon-out/structure.json
```

Roughly 45 requests, spaced 600 ms apart as the collector spaces its own. The run
aborts before probing anything unknown if the known-live control endpoints do not
respond, so a report of uniform `404`s from a host that is merely down cannot be
mistaken for a host with no write surface.

`recon-out/` is gitignored. The structural report may be committed deliberately
once read; the raw bodies beside it are not this repository's to redistribute.
