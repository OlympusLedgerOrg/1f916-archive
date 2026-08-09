#!/usr/bin/env bash
# Seal the cumulative packet set into a new dataset-manifest version.
#
# The commitment layer is the Olympus ADR-0027 dataset manifest, used exactly as
# shipped: `olympus build` hashes every packet, `olympus diff` seals the new
# version against its parent, and `olympus link` verifies the resulting version
# link before anything is signed.
#
# Because packets are immutable and only ever added, every diff is strictly
# additive and therefore fully explains its root delta. That is not a stylistic
# choice. `compute_diff` keys on (shard_id, record_id, version) and tests
# presence only — it never compares content_hash for a key present on both
# sides — and `verify_link` does not check that the change set explains the root
# delta. A layout that overwrote a record in place would therefore produce an
# EMPTY diff between two DIFFERENT roots, and the link would still verify as
# Valid while the earlier bytes were gone. Capture sequencing is what makes this
# check mean something.
#
# Usage: scripts/build-manifest.sh [--force]
set -euo pipefail

cd "$(dirname "$0")/.."

FORCE=0
[[ "${1:-}" == "--force" ]] && FORCE=1

OLYMPUS_BIN="${OLYMPUS_BIN:-olympus}"
# Rust glue instead of `jq` or a Python interpreter: this repository is a Rust
# pipeline and should not need a second language runtime to read a JSON field.
COLLECT_BIN="${COLLECT_BIN:-./target/release/f916-collect}"
DATASET_ID="1f916-archive"
ARCHIVE_DIR="archive"
MANIFEST_DIR="artifacts/manifests"
INDEX_DIR="artifacts/indexes"
DIFF_DIR="artifacts/diffs"
LATEST="artifacts/latest.json"

command -v "$OLYMPUS_BIN" >/dev/null 2>&1 || [[ -x "$OLYMPUS_BIN" ]] || {
  echo "error: '$OLYMPUS_BIN' not found. Install the pinned Olympus CLI first:" >&2
  echo "       scripts/install-olympus-cli.sh" >&2
  exit 1
}
[[ -x "$COLLECT_BIN" ]] || { echo "error: '$COLLECT_BIN' not built (cargo build --release)" >&2; exit 1; }
[[ -d "$ARCHIVE_DIR" ]] || { echo "error: no $ARCHIVE_DIR directory; run a capture first" >&2; exit 1; }

json() { "$COLLECT_BIN" json --file "$1" --field "$2" ${3:+--len}; }

mkdir -p "$MANIFEST_DIR" "$INDEX_DIR" "$DIFF_DIR"

# Nothing new means nothing to seal: a version adding no records would be a
# root-identical duplicate carrying an empty diff, which is noise in the chain.
if [[ $FORCE -eq 0 ]] && [[ -z "$(git status --porcelain -- "$ARCHIVE_DIR")" ]]; then
  echo "no new packets in $ARCHIVE_DIR; nothing to seal (use --force to override)"
  exit 0
fi

PARENT_VERSION=0
PARENT_ROOT=""
if [[ -f "$LATEST" ]]; then
  PARENT_VERSION=$(json "$LATEST" version)
  PARENT_ROOT=$(json "$LATEST" manifest_root)
fi
VERSION=$((PARENT_VERSION + 1))
TAG=$(printf 'v%06d' "$VERSION")

MANIFEST="$MANIFEST_DIR/$TAG.json"
INDEX="$INDEX_DIR/$TAG.json"

[[ -e "$MANIFEST" ]] && {
  echo "error: $MANIFEST already exists; refusing to overwrite a sealed version" >&2
  exit 1
}

echo "sealing $DATASET_ID v$VERSION"
"$OLYMPUS_BIN" build \
  --data "$ARCHIVE_DIR" \
  --dataset-id "$DATASET_ID" \
  --version "$VERSION" \
  --shard-from-subdir \
  --out "$MANIFEST" \
  --index "$INDEX" \
  --name "1f916.ai verifiable archive" \
  --description "Immutable capture packets of public 1f916.ai API responses. A commitment to received bytes only; see the README section 'Captured, committed, truthful'." \
  --license "Archive tooling: Apache-2.0. Captured payloads: rights retained by their authors." \
  --source "https://1f916.ai" \
  --parser-id "1f916-archive-collector" \
  --parser-version "capture-meta/v1" \
  --model-hash "none"

if [[ "$PARENT_VERSION" -gt 0 ]]; then
  PARENT_MANIFEST="$MANIFEST_DIR/$(printf 'v%06d' "$PARENT_VERSION").json"
  PARENT_INDEX="$INDEX_DIR/$(printf 'v%06d' "$PARENT_VERSION").json"
  DIFF="$DIFF_DIR/$TAG.json"
  echo "diffing against v$PARENT_VERSION"
  "$OLYMPUS_BIN" diff \
    --parent-manifest "$PARENT_MANIFEST" \
    --parent-index "$PARENT_INDEX" \
    --child-manifest "$MANIFEST" \
    --child-index "$INDEX" \
    --out-child "$MANIFEST" \
    --out-diff "$DIFF"

  # Verify the link before anything is signed. A version whose link does not
  # check out must never reach the transparency log.
  "$OLYMPUS_BIN" link \
    --child "$MANIFEST" \
    --parent-version "$PARENT_VERSION" \
    --parent-root "$PARENT_ROOT" \
    --diff "$DIFF"

  # Packets are immutable, so the packet set only grows — unless a payload was
  # deliberately withheld, which does remove that record from *later* indexes
  # while leaving every earlier manifest and anchor untouched.
  #
  # In a diff, a declared withholding and evidence quietly vanishing look
  # identical. This is what tells them apart: every removal must be registered,
  # naming this exact version. An undeclared one stops the build before anything
  # is signed.
  "$COLLECT_BIN" removals --diff "$DIFF" --withheld withheld.json
fi

ROOT=$(json "$MANIFEST" manifest_root)
RECORDS=$(json "$MANIFEST" record_count)

[[ "$ROOT" =~ ^[0-9a-f]{64}$ ]] || { echo "error: implausible manifest_root '$ROOT'" >&2; exit 1; }
[[ "$RECORDS" =~ ^[0-9]+$ ]] || { echo "error: implausible record_count '$RECORDS'" >&2; exit 1; }

cat > "$LATEST" <<EOF
{
  "index": "$INDEX",
  "manifest": "$MANIFEST",
  "manifest_root": "$ROOT",
  "record_count": $RECORDS,
  "version": $VERSION
}
EOF

echo
echo "sealed $TAG"
echo "  manifest_root: $ROOT"
echo "  records:       $RECORDS"
echo "  manifest:      $MANIFEST"
