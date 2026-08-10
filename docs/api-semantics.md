# `/api/changes` semantics, measured

**Method:** read-only. A full drain of `/api/changes` from `since=0` following
`next_since` (7 pages), the complete ascending `/api/events` log, and targeted
`/api/post/{id}` fetches. No writes to 1f916 of any kind.

**Date of measurement:** 2026-08-09. Every number below is from that run; the
API is someone else's and may change without notice.

The question this answers: does `/api/changes` report *edits* to existing posts
and *comment-only* changes, or only new content? The endpoint's own
documentation does not say, and the answer decides whether the collector needs a
bounded periodic re-fetch policy.

---

## 1. The cursor is a `created_at` watermark

`next_since` equalled `min(max(posts[].created_at), max(comments[].created_at))`
on all 7 pages:

| page | posts | comments | `max` posts | `max` comments | `next_since` | `has_more` |
|---:|---:|---:|---:|---:|---:|:--|
| 1 | 200 | 500 | 1786059214657 | 1786033222133 | 1786033222133 | true |
| 2 | 200 | 500 | 1786152095432 | 1786063348256 | 1786063348256 | true |
| 3 | 200 | 500 | 1786208520218 | 1786109695812 | 1786109695812 | true |
| 4 | 200 | 500 | 1786281961016 | 1786170141202 | 1786170141202 | true |
| 5 | 134 | 500 | 1786298046565 | 1786226294259 | 1786226294259 | true |
| 6 |  82 | 500 | 1786298046565 | 1786286074799 | 1786286074799 | true |
| 7 |  18 | 168 | 1786298046565 | 1786300323573 | 1786300325778 | false |

Taking the *minimum* of the two watermarks is the correct conservative choice:
the array that hit its cap first bounds how far the cursor may move, so no row
in the other array is skipped. The cost is duplication, which is exactly what the
endpoint's `cursor_note` warns about.

Measured duplication over the full drain: **1034 post rows → 504 unique (51.3%
duplicated); 3168 comment rows → 3168 unique (0% duplicated).** The asymmetry is
structural, not incidental — the comments array hits its 500-row cap first on
every page, so the cursor tracks the comment watermark and posts past it are
re-sent. Rows must be treated as **upserts by id**.

## 2. Edits to existing posts are not reported — and cannot be

Two independent reasons, one structural and one empirical.

**Structural.** The `/api/changes` posts row is exactly:

```
{ id, title, url, created_at, author, author_model }
```

No `body`. No `votes`, `flags`, `pinned`, or `mod_state`. And no `updated_at` or
`edited_at` — *not anywhere in the API*, including `/api/post/{id}`, which
returns `{ id, title, body, url, pinned, mod_state, created_at, author,
author_model, votes, flags }` and no modification timestamp at all. Since the
cursor is derived from `created_at` (§1), a mutation of an existing row has
nothing that could move it past a cursor already beyond its creation time.

**Empirical.** Eight comments carry a non-null `mod_state`. Every corresponding
moderation event strictly postdates the comment's creation:

| comment | created → moderated | latency |
|---:|---|---:|
| 1014 | 1786064376550 → 1786065338755 | 16.0 min |
| 787 | 1786049618042 → 1786052274646 | 44.3 min |
| 782 | 1786049470419 → 1786052274968 | 46.7 min |
| 780 | 1786049218449 → 1786052274806 | 50.9 min |
| 2890 | 1786264090556 → 1786267468477 | 56.3 min |
| 2839 | 1786260907642 → 1786267468585 | 109.3 min |
| 2678 | 1786246102066 → 1786253963330 | 131.0 min |
| 2650 | 1786242492404 → 1786253963194 | 191.2 min |

Each of those comments appeared **exactly once** across the whole drain, at its
`created_at` position. Zero comment-id duplicates in 3168 rows. A state change
does not re-emit a row.

## 3. New comments *are* reported

`comments[]` is a first-class array carrying `post_id`, and it advances the
cursor. So comment-only *activity* on an old post is discoverable: the posts whose
thread state changed in a window are exactly the `post_id`s on that window's
comment rows. This is the one mutation class the feed does surface, and the
collector uses it to target re-fetches precisely.

## 4. The feed serves *current* row state, positioned at `created_at`

Comment 780 was delivered with `mod_state: "removed"` and a 71-byte placeholder
body, at its original `created_at` position. So a re-drain from `since=0` returns
today's state, not the state at creation time.

Two consequences. A full re-drain is a cheap *present-state* snapshot. And it can
never recover bytes already destroyed upstream — moderation replaces the original
title and body rather than flagging them.

## 5. `/api/changes` omits moderated posts entirely

504 unique posts were discovered, but ids run 1..513 with 9 gaps. Every gap was
probed directly:

| ids | `/api/post/{id}` | state |
|---|---|---|
| 2, 27 | **404** | hard-deleted; no tombstone in changes or events |
| 179, 189 | 200 | `mod_state: "removed"` — title and body both replaced by a 71-char placeholder |
| 66, 70, 500, 507, 508 | 200 | `mod_state: "collapsed"` — title and body both replaced by a 122-char placeholder |

Not one post with a non-null `mod_state` appeared anywhere in the drained feed.
Moderated *comments* stay in the feed (with redacted bodies); moderated *posts*
are dropped from it.

**This is the finding that shapes the collector.** A post moderated before the
next poll is not merely reported late — it is never discoverable through
`/api/changes` at all. At the time of measurement 5 of 513 posts (~1%) were in
that state, plus 2 hard-deleted. With a median moderation latency of ~56 minutes
(§2), an hourly collector will miss some of them, and a daily one will miss most.

## 6. `/api/events` must be drained unfiltered

The default view is the newest 500, descending. `?since=0` returns the complete
log ascending (71 events total, 39 of kind `moderation`).

The archive captures the **unfiltered** log rather than `?kind=moderation`,
because the hash chain spans kinds: moderation event 65's `prev_hash` matches
event 64, which is not a moderation event. A filtered page therefore contains
`prev_hash` values pointing at rows it does not include, and the chain cannot be
verified from it. Capturing the complete log is what makes the chain checkable
offline.

Separately, events 1–13 carry `prev_hash: null` and `hash: null` — the chain
begins at event 14/15, where event 15's `prev_hash` is 64 zeroes. Anything before
that point is unchained by the site's own construction.

## 7. `/api/attest` is paginated, and complete only by a wide margin

Measured 2026-08-10 from the archive's own captured packets, sequences 20–25 —
counters only, no prose read:

| | `page_size` | `total_rows` | `verified_through_id` | `sealed` | `unsealed` |
|---|---:|---:|---:|---:|---:|
| `identity_log` | 20 000 | 83 | 83 | 69 | 14 |
| `treasury` | 20 000 | 13 | 13 | 5 | — |

Every capture is complete: `verified_through_id` equals `total_rows` on both
chains, and `ok` is true. **Nothing in the archive is a truncated attestation.**

Two things follow.

**The counters independently corroborate §6.** 14 unsealed entries with
`sealed_from_id: 15` is the same boundary §6 derived from `/api/events` — that the
chain begins at event 14/15 and everything before it is unchained by the site's
own construction. Two endpoints, one conclusion, arrived at separately.

**The margin is what makes this safe, and margins close.** `page_size` is 20 000
against 83 rows growing at roughly 1.6 per hour: about 500 days of headroom. The
collector fetches `/api/attest` exactly once and has no continuation handling, so
on the day a chain outgrows one page it would store page one and the archive would
have an unstated incompleteness — the single thing this project is not allowed to
have.

So the collector now reads the two coverage counters after every capture and warns
if either chain was covered only in part (`src/api.rs`, `ChainCoverage`). The
check is integers only, like everything else in that module; coverage is decidable
from the counters, so no status string crosses into the program. Pagination itself
is deliberately *not* implemented: it would be speculative work against a shape
nobody has observed, and a loud warning is worth more today than an untested code
path for a response that has never arrived.

---

## What the collector does about all this

| finding | collector behaviour |
|---|---|
| cursor is `created_at`-keyed (§1) | advance to `next_since`, never `now`; drain while `has_more`; treat rows as upserts by id |
| boundary rows repeat (§1) | dedupe discovery within a run; a repeat capture across runs is written to a new sequence, never over an old packet |
| edits unreportable (§2) | bounded rotation re-fetch over the corpus, so a state transition is eventually captured as a *second* packet |
| new comments reported (§3) | re-fetch exactly the posts whose `post_id` appeared on comment rows this window |
| moderated posts invisible (§5) | integer id sweep: probe known gaps, then probe forward past the highest known id until a run of 404s |
| deletions untombstoned (§5) | a 404 below the highest-known-present id is recorded as confirmed absence; above it, never cached |
| chain spans kinds (§6) | capture `/api/events?since=0` unfiltered |
| attest pages (§7) | read the coverage counters after each capture; warn loudly if either chain was only partly covered, rather than storing a prefix in silence |

None of this makes the archive complete. It makes the *incompleteness* bounded
and stated: what the archive holds is what it managed to fetch before someone
upstream changed it.
