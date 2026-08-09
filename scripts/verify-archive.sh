#!/usr/bin/env bash
# Independent verification of everything this archive claims.
#
# Four checks, in increasing order of what they establish:
#
#   1. REPRODUCE  — rebuild the manifest from the packets on disk and confirm the
#                   recorded manifest_root comes back. This needs no signature
#                   and no network: it says the commitment matches the bytes.
#   2. LINK       — verify every version link against its parent root and diff,
#                   and confirm no version ever removed a record.
#   3. ANCHOR     — verify each Sigstore bundle against the PINNED workflow
#                   identity and OIDC issuer.
#   4. NEGATIVE   — confirm the anchor check actually rejects a wrong identity, a
#                   wrong issuer, and a tampered manifest.
#
# Check 4 is not ceremony. A verifier that prints a Rekor log index and exits 0
# looks identical, in a terminal, to one that verifies nothing. The only way to
# know a check has teeth is to watch it bite.
#
# Usage: scripts/verify-archive.sh [--require-anchors]
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/anchor-identity.sh
source scripts/anchor-identity.sh

REQUIRE_ANCHORS=0
[[ "${1:-}" == "--require-anchors" ]] && REQUIRE_ANCHORS=1

OLYMPUS_BIN="${OLYMPUS_BIN:-olympus}"
COLLECT_BIN="${COLLECT_BIN:-./target/release/f916-collect}"
MANIFEST_DIR="artifacts/manifests"
INDEX_DIR="artifacts/indexes"
DIFF_DIR="artifacts/diffs"
BUNDLE_DIR="artifacts/bundles"

fail() { echo "FAIL: $*" >&2; exit 1; }
json() { "$COLLECT_BIN" json --file "$1" --field "$2" ${3:+--len}; }

[[ -x "$COLLECT_BIN" ]] || fail "$COLLECT_BIN not built (cargo build --release)"
command -v "$OLYMPUS_BIN" >/dev/null 2>&1 || [[ -x "$OLYMPUS_BIN" ]] || \
  fail "$OLYMPUS_BIN not found (scripts/install-olympus-cli.sh)"

shopt -s nullglob
MANIFESTS=("$MANIFEST_DIR"/v*.json)
shopt -u nullglob
[[ ${#MANIFESTS[@]} -gt 0 ]] || fail "no manifests in $MANIFEST_DIR"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# ── 1. reproduce the newest root from the packets ───────────────────────────
LATEST_MANIFEST="${MANIFESTS[-1]}"
LATEST_VERSION=$(json "$LATEST_MANIFEST" version)
EXPECTED_ROOT=$(json "$LATEST_MANIFEST" manifest_root)

echo "== 1. reproduce v$LATEST_VERSION from archive/ =="
"$OLYMPUS_BIN" build \
  --data archive \
  --dataset-id "$(json "$LATEST_MANIFEST" dataset_id)" \
  --version "$LATEST_VERSION" \
  --shard-from-subdir \
  --out "$WORK/rebuilt.json" \
  --index "$WORK/rebuilt-index.json" \
  --parser-id "1f916-archive-collector" \
  --parser-version "capture-meta/v1" \
  --model-hash "none" >/dev/null

REBUILT_ROOT=$(json "$WORK/rebuilt.json" manifest_root)
[[ "$REBUILT_ROOT" == "$EXPECTED_ROOT" ]] || \
  fail "rebuilt root $REBUILT_ROOT != recorded $EXPECTED_ROOT"
echo "   ok: $EXPECTED_ROOT reproduces from the packets on disk"
echo "   (manifest_root is a pure function of the record set and its provenance;"
echo "    it does not depend on created_at, so this is exactly reproducible)"

# ── 2. version links ─────────────────────────────────────────────────────────
echo
echo "== 2. version links =="
if [[ ${#MANIFESTS[@]} -eq 1 ]]; then
  echo "   only v1 exists; no links to check yet"
else
  for i in $(seq 1 $((${#MANIFESTS[@]} - 1))); do
    PARENT="${MANIFESTS[$((i - 1))]}"
    CHILD="${MANIFESTS[$i]}"
    CHILD_VERSION=$(json "$CHILD" version)
    DIFF="$DIFF_DIR/$(printf 'v%06d' "$CHILD_VERSION").json"
    [[ -f "$DIFF" ]] || fail "missing diff for v$CHILD_VERSION"
    "$OLYMPUS_BIN" link \
      --child "$CHILD" \
      --parent-version "$(json "$PARENT" version)" \
      --parent-root "$(json "$PARENT" manifest_root)" \
      --diff "$DIFF" >/dev/null || fail "link check failed for v$CHILD_VERSION"

    # `verify_link` checks the ParentRef binding and the diff summary, but not
    # that the change set explains the root delta — so a removal passes it
    # silently. Every removal must therefore be a registered withholding.
    "$COLLECT_BIN" removals --diff "$DIFF" --withheld withheld.json >/dev/null \
      || fail "v$CHILD_VERSION removes records that are not declared in withheld.json"
    echo "   ok: v$(json "$PARENT" version) -> v$CHILD_VERSION (+$(json "$DIFF" added --len), -$(json "$DIFF" removed --len) declared)"
  done
fi

# ── 3 & 4. anchors, and proof that the anchor check has teeth ───────────────
echo
echo "== 3. Sigstore anchors =="
echo "   identity: $ANCHOR_IDENTITY"
echo "   issuer:   $ANCHOR_ISSUER"

shopt -s nullglob
BUNDLES=("$BUNDLE_DIR"/v*.sigstore.json)
shopt -u nullglob

if [[ ${#BUNDLES[@]} -eq 0 ]]; then
  [[ $REQUIRE_ANCHORS -eq 1 ]] && fail "no bundles in $BUNDLE_DIR and --require-anchors was given"
  echo "   no bundles yet — the archive is internally tamper-evident but NOT"
  echo "   independently time-attested. Re-run with --require-anchors to make"
  echo "   this a failure."
  echo
  echo "ALL CHECKS PASSED (anchors not present)"
  exit 0
fi

command -v cosign >/dev/null || fail "bundles exist but cosign is not on PATH"

verify_bundle() {  # bundle manifest identity issuer
  cosign verify-blob \
    --bundle "$1" \
    --new-bundle-format \
    --certificate-identity "$3" \
    --certificate-oidc-issuer "$4" \
    "$2" >/dev/null 2>&1
}

for BUNDLE in "${BUNDLES[@]}"; do
  TAG=$(basename "$BUNDLE" .sigstore.json)
  MANIFEST="$MANIFEST_DIR/$TAG.json"
  [[ -f "$MANIFEST" ]] || fail "$BUNDLE has no matching manifest $MANIFEST"

  # A bundle with no transparency-log entry would still verify offline against a
  # certificate, but it would carry no independent timestamp — which is the only
  # reason this archive signs anything.
  grep -q '"logIndex"' "$BUNDLE" || fail "$BUNDLE carries no Rekor transparency-log entry"

  verify_bundle "$BUNDLE" "$MANIFEST" "$ANCHOR_IDENTITY" "$ANCHOR_ISSUER" \
    || fail "$TAG does not verify against the pinned identity"
  echo "   ok: $TAG verifies"
done

echo
echo "== 4. negative controls =="
PROBE_BUNDLE="${BUNDLES[-1]}"
PROBE_TAG=$(basename "$PROBE_BUNDLE" .sigstore.json)
PROBE_MANIFEST="$MANIFEST_DIR/$PROBE_TAG.json"

# Wrong identity: a signature from any other workflow, repository or ref must be
# rejected. If this passes, the identity pin is decorative.
if verify_bundle "$PROBE_BUNDLE" "$PROBE_MANIFEST" \
     "https://github.com/attacker/evil/.github/workflows/x.yml@refs/heads/main" "$ANCHOR_ISSUER"; then
  fail "a wrong certificate-identity verified — the identity pin is not enforced"
fi
echo "   ok: wrong certificate-identity is rejected"

# Wrong issuer: a token from any other OIDC provider must be rejected.
if verify_bundle "$PROBE_BUNDLE" "$PROBE_MANIFEST" \
     "$ANCHOR_IDENTITY" "https://accounts.google.com"; then
  fail "a wrong OIDC issuer verified — the issuer pin is not enforced"
fi
echo "   ok: wrong OIDC issuer is rejected"

# Tampered blob: the signature is over the manifest bytes, so a single changed
# byte must break it.
cp "$PROBE_MANIFEST" "$WORK/tampered.json"
printf ' ' >> "$WORK/tampered.json"
if verify_bundle "$PROBE_BUNDLE" "$WORK/tampered.json" "$ANCHOR_IDENTITY" "$ANCHOR_ISSUER"; then
  fail "a tampered manifest verified — the signature does not bind the bytes"
fi
echo "   ok: tampered manifest is rejected"

echo
echo "ALL CHECKS PASSED"
echo
echo "What this establishes: these bytes were captured, their hashes are"
echo "committed in the named manifest versions, and those manifests were signed"
echo "by this repository's workflow identity and recorded in a public"
echo "transparency log. It establishes nothing about whether the captured"
echo "content is TRUE. See the README."
