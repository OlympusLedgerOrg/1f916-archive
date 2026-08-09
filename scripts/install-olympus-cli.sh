#!/usr/bin/env bash
# Install the Olympus dataset-manifest CLI at a pinned revision.
#
# `clients/cli` is deliberately a standalone crate but it is NOT published to
# crates.io, and it uses workspace-relative path dependencies on
# `crates/olympus-manifest` and `crates/olympus-crypto`. So it cannot be
# installed with `cargo install olympus-cli`, and `cargo install --git` cannot
# find it either: it is its own workspace root, not a member of the repository's
# root workspace, so cargo's package search never reaches it.
#
# Clone at an exact commit and `cargo install --locked --path` instead. The
# commit is the archive's dependency pin: the hashing rules that produce a
# manifest root live in that revision, and a different revision could produce a
# different root for the same bytes.
set -euo pipefail

# Pin. Update deliberately, and re-verify that historical roots still reproduce
# before committing a change.
OLYMPUS_REPO="${OLYMPUS_REPO:-https://github.com/OlympusLedgerOrg/Olympus.git}"
OLYMPUS_REV="${OLYMPUS_REV:-1537c77860a6d85fd20592b8f3248fd363a20616}"

DEST="${1:-${RUNNER_TEMP:-/tmp}/olympus-cli}"
SRC="$DEST/src"

mkdir -p "$SRC"
if [[ ! -d "$SRC/.git" ]]; then
  git init -q "$SRC"
  git -C "$SRC" remote add origin "$OLYMPUS_REPO"
fi
git -C "$SRC" fetch --depth 1 origin "$OLYMPUS_REV"
git -C "$SRC" checkout -q FETCH_HEAD

# Trust nothing about the fetch: confirm the tree we are about to build from is
# the revision we pinned.
ACTUAL=$(git -C "$SRC" rev-parse HEAD)
if [[ "$ACTUAL" != "$OLYMPUS_REV" ]]; then
  echo "error: checked out $ACTUAL but pinned $OLYMPUS_REV" >&2
  exit 1
fi

# --locked so the committed clients/cli/Cargo.lock governs the dependency graph.
cargo install --locked --path "$SRC/clients/cli" --root "$DEST" --force

echo
echo "installed olympus CLI from $OLYMPUS_REV"
"$DEST/bin/olympus" help | head -3
echo "add to PATH:  export PATH=\"$DEST/bin:\$PATH\""
