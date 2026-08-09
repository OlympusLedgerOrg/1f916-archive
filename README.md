# 1f916.ai verifiable archive

An independent, externally anchored custody record for the public content of
[1f916.ai](https://1f916.ai).

1f916 hash-chains its own events and treasury ledgers and exposes `/api/attest`
to check them. Its attest endpoint is unusually candid about what that proves:
nothing, if you only ever ask 1f916. Whoever holds the database could rewrite
history, recompute the chains to match, and the endpoint would report a clean
chain while truthfully describing a history that had changed.

That is an accurate description of an unfalsifiable claim. This archive is the
witness it says it lacks — not a judge of whether 1f916 is honest, but a way to
make dishonesty leave a mark that neither party controls.

---

## Captured, committed, truthful

Three propositions. The archive can establish the first two. It cannot establish
the third, and no output of this repository should be read as if it could.

| | proposition | who establishes it |
|---|---|---|
| **1. CAPTURED** | The collector received *these bytes* from *this endpoint* at *this time*. | This archive, directly. |
| **2. COMMITTED** | That byte string's hash was incorporated into a named manifest version and recorded in a public transparency log at a specific time. | This archive, directly. |
| **3. TRUTHFUL** | The bytes accurately describe reality — or even accurately represent what the upstream system would say about itself. | **Nobody here. Outside this archive's authority entirely.** |

The distinction is sharpest exactly where the archive is most useful. A captured
`/api/attest` packet proves what 1f916 **claimed** at time T. It never proves the
claim was correct. What it does is convert an unfalsifiable claim into a
falsifiable one: a later rewrite must either reproduce the heads that were
published before it, or contradict a transparency-log record that predates it.

A proof from this archive says a document with a specific hash occupied a
specific capture path in a specific manifest version, anchored at a specific
time. Read it as nothing more.

---

## How it works

```text
HTTPS fetch (bounded) ──▶ immutable packet ──▶ manifest root ──▶ Sigstore/Rekor
   size + time caps        never overwritten     Olympus SMT       keyless, OIDC
```

### Immutable capture packets

Every response is stored verbatim under a locally generated path. Nothing is
ever overwritten:

```text
archive/post/captures/000001/506.json        the raw /api/post/506 response
archive/post/captures/000001/506.meta.json   collector observations + BLAKE3
archive/site/captures/000001/attest.json     what 1f916 claimed, this run
archive/changes/captures/000001/page-0001.json
```

Each run allocates a new capture sequence. A post re-fetched later lands in a new
directory beside the old one, so the *pair* of packets is the evidence of an
upstream edit, moderation, or deletion.

This is not stylistic. The mutable alternative — one `post/{id}.json`, overwritten
each capture — would be certified **incorrectly** by the tooling. `olympus`
assigns `version: 1` to every record and uses the full relative path as the
record id; `compute_diff` keys on `(shard_id, record_id, version)` and tests
presence only, never comparing content hashes for a key present on both sides. An
edited post would keep its tree key and change its value, landing in neither
`added` nor `removed`. `verify_link` checks the parent binding and the diff
summary but not that the change set explains the root delta. The result would be
an **empty diff between two different manifest roots that verifies as Valid**,
while the earlier bytes were gone. Capture sequencing makes every version
strictly additive, so each diff fully explains its root delta.

### Commitment

The commitment layer is the Olympus ADR-0027 dataset manifest, used exactly as
shipped and pinned to an exact Olympus commit. Each capture seals a new version
over the cumulative packet set, diffs it against its parent, and verifies the
version link before anything is signed.

### Anchoring

Manifests are signed with keyless Sigstore in the scheduled GitHub Action. There
is no long-lived key anywhere in this project: the workflow presents a short-lived
OIDC token, Fulcio issues a certificate bound to the workflow identity, and Rekor
records it in a public transparency log.

Verification **pins** the expected workflow identity and OIDC issuer. Without
that pin, a bundle check proves only that somebody signed the blob and Sigstore
logged it, which is true of any attacker with a GitHub account.

### Untrusted content

Every response is treated as adversarial bytes. The archive never renders,
executes, templates, shell-interpolates, summarises, classifies, or sends
captured content to a language model. Two rules are enforced mechanically rather
than by convention:

- [`src/api.rs`](src/api.rs) contains **no `String` field**. The only values taken
  out of a response are integers. Upstream text has nowhere to go.
- [`src/packet.rs`](src/packet.rs) builds every filename from a compile-time
  constant or a locally formatted integer. `PacketName` has no variant that
  accepts caller-supplied text, so a traversing path cannot be spelled even
  deliberately.

The collector sends an identifying `User-Agent` with a contact URL, caps body
size and time, rejects unexpected content types before reading a body, spaces its
requests, retries with bounded jitter, and honours `429` and `Retry-After`. It is
an indefinite guest of someone else's public infrastructure.

---

## What this archive does *not* capture

Measured against the live API, not assumed. Full workings in
[`docs/api-semantics.md`](docs/api-semantics.md).

`/api/changes` is a `created_at` watermark feed. No row in the API carries an
update timestamp, so:

- **Post and comment edits are never reported.** A bounded rotation re-fetch
  catches them eventually, as a second packet — not at the moment they happen.
- **Moderated posts are omitted from the feed entirely.** A post that is
  collapsed or removed disappears from `/api/changes`; at the time of
  measurement, 5 of 513 posts were in that state. An integer id sweep is the only
  way to reach them, so the collector has one.
- **Deletions leave no tombstone.** Two post ids simply return 404. The collector
  records confirmed absence, so a gap is a recorded fact rather than a silence.
- **Moderation destroys the upstream bytes**, replacing title and body with a
  placeholder. Median observed latency from creation to moderation was ~56
  minutes, which is why capture runs hourly. What the archive holds is what it
  managed to fetch first.

The archive is not complete, and does not claim to be. Its incompleteness is
bounded and stated.

---

## Verifying it yourself

```bash
scripts/install-olympus-cli.sh ~/olympus     # pinned Olympus commit
export PATH="$HOME/olympus/bin:$PATH"
cargo build --release
scripts/verify-archive.sh --require-anchors
```

That runs four checks:

1. **Reproduce** — rebuild the manifest from the packets on disk and confirm the
   recorded root comes back. The root is a pure function of the record set and
   its provenance, so this is exact.
2. **Link** — verify every version link against its parent root and diff, and
   confirm no version removed a record undeclared in `withheld.json`.
3. **Anchor** — verify each Sigstore bundle against the pinned identity and
   issuer.
4. **Negative controls** — confirm a wrong identity, a wrong issuer, and a
   tampered manifest are each *rejected*.

The fourth is not ceremony. A verifier that prints a Rekor log index and exits 0
looks identical, in a terminal, to one that verifies nothing.

### Proving a single record

```bash
olympus prove --manifest artifacts/manifests/v000001.json \
              --index artifacts/indexes/v000001.json \
              --shard post --record post/captures/000001/179.json \
              --out /tmp/proof.json
olympus verify --proof /tmp/proof.json --manifest artifacts/manifests/v000001.json
```

Post 179 is one of the moderated posts `/api/changes` will never mention.

---

## Withheld payloads

Commitments and payloads are governed separately. Proof verification never reads
the payload, so a withheld payload invalidates no proof, breaks no version link,
and changes no root. Withholding is **not** a statement that a commitment is
invalid, disputed, or withdrawn — only that redistribution stops.

Every withholding is registered in [`WITHHELD.md`](WITHHELD.md) and enforced by
`withheld.json`: an undeclared removal fails the build before anything is signed,
and fails verification for anyone who checks afterwards. A withheld payload is
*visibly* withheld, never silently absent.

---

## Relationship to Olympus

This repository is deliberately separate from
[OlympusLedgerOrg/Olympus](https://github.com/OlympusLedgerOrg/Olympus). It
consumes `clients/cli` at a pinned commit and changes no Olympus invariant: no
new leaf layout or domain, no new shard in the production ledger, no frontend
crypto, and no LLM processing. 1f916's volatile third-party API has no business
in the desktop ledger's audit, availability, or release surface.

## Licence

Archive tooling: Apache-2.0 (see [LICENSE](LICENSE)).

Captured payloads are the work of their authors and are reproduced here as
evidence of what a public endpoint served at a given time. No licence over that
content is claimed or granted.
