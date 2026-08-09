# Withheld payloads

A public register of capture packets whose **bytes** have been removed from this
repository. It is currently empty.

## What withholding does and does not do

The archive has two independently governed layers:

| layer | contents | governance |
|---|---|---|
| **commitment** | manifests, record indexes, diffs, Sigstore bundles | append-only; never rewritten, never withdrawn |
| **payload** | the captured response bytes | may be withheld or delisted per record |

These are separable because **proof verification never reads the payload.** A
record proof carries the committed content hash and the authenticated tree path;
verification re-derives the tree key from `(shard_id, record_id, version)` and
folds that path to the manifest root. Withholding a payload therefore invalidates
no proof, breaks no version link, and changes no root.

You can watch this yourself:

```bash
olympus prove --manifest artifacts/manifests/v000001.json \
              --index artifacts/indexes/v000001.json \
              --shard post --record post/captures/000001/179.json --out /tmp/p.json
mv archive/post/captures/000001/179.json /tmp/withheld    # payload gone
olympus verify --proof /tmp/p.json --manifest artifacts/manifests/v000001.json
# VALID INCLUSION
```

What a withheld record still establishes: a document with a specific hash
occupied a specific capture path in a specific manifest version, anchored at a
specific time. What is lost is a *new* reader's ability to reconstruct the
content — not anyone's ability to establish what existed. Anyone who
independently retained the bytes can still check them against the published
commitment.

**Withholding does not imply the commitment is invalid, disputed, or withdrawn.**
The commitment stands exactly as it did before. Only redistribution stops.

## Policy

Deliberately narrow, and stated as discretion rather than obligation.

- The archive **may** withhold or delist payload bytes. It does not commit to
  honouring every request, and this document is not a promise to do so.
- Withholding is a maintainer decision. Plausible triggers include an upstream
  moderation removal, a credible legal demand, or content whose continued
  republication is indefensible on its own terms. The enumeration is not a
  guarantee and each case is judged individually.
- Every withheld payload is recorded below. A withheld payload is **visibly**
  withheld, never silently absent — an archive that could quietly drop records
  would forfeit the property it exists to provide.

This is a governance boundary, not a protocol. It defines no mechanism, no
cryptographic construction, and no automated enforcement.

## What a withholding does to future manifests

Each capture seals a manifest over the packets that exist at that moment. So
deleting a payload does have a visible effect: that record leaves the *next*
index, and the next diff shows a removal. Every manifest sealed before the
withholding, and every anchor over them, is untouched and still verifies.

In a diff, a declared withholding and evidence quietly vanishing look identical.
That is not a distinction a README can enforce, so it is enforced in code
instead: `f916-collect removals` requires every removed record to be declared in
`withheld.json` **at that exact version**, and both `scripts/build-manifest.sh`
and `scripts/verify-archive.sh` run it. An undeclared removal fails the build
before anything is signed, and fails verification for anyone who checks later.

Pinning the version matters: without it, one stale entry would permanently
authorise removing that record again in any later version, with no one having to
re-declare it.

## Procedure

1. Delete the payload file. Leave the `.meta.json` sidecar in place. The sidecar
   carries the capture metadata and the collector's BLAKE3 of the withheld
   bytes — and it is itself a committed record, so it goes on attesting, under
   the same anchored roots, that those bytes were received and what they hashed
   to.
2. Add an entry to `withheld.json` with the shard, the full record id, the
   version the next seal will produce, the date, and a reason category.
3. Add the matching row to the register below.
4. Do **not** rebuild, re-seal, or re-anchor any past manifest. The commitment
   layer is append-only; this procedure only stops redistribution.

## Register

| record path | withheld at version | date | reason category |
|---|---|---|---|
| _(none)_ | | | |
