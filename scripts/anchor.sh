#!/usr/bin/env bash
# Anchor the newest sealed manifest with keyless Sigstore signing.
#
# No long-lived key exists anywhere in this project. The GitHub Actions run
# presents a short-lived OIDC token, Fulcio issues a certificate bound to the
# workflow identity, and Rekor records the signature in a public transparency
# log. What that buys the archive is the one thing its own Git history cannot
# provide: a timestamped record held by a party neither the archive nor 1f916
# controls.
#
# Usage: scripts/anchor.sh          (inside a workflow with id-token: write)
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/anchor-identity.sh
source scripts/anchor-identity.sh

COLLECT_BIN="${COLLECT_BIN:-./target/release/f916-collect}"
LATEST="artifacts/latest.json"
BUNDLE_DIR="artifacts/bundles"

command -v cosign >/dev/null || { echo "error: cosign not on PATH" >&2; exit 1; }
[[ -f "$LATEST" ]] || { echo "error: no $LATEST; seal a manifest first" >&2; exit 1; }

MANIFEST=$("$COLLECT_BIN" json --file "$LATEST" --field manifest)
VERSION=$("$COLLECT_BIN" json --file "$LATEST" --field version)
TAG=$(printf 'v%06d' "$VERSION")
BUNDLE="$BUNDLE_DIR/$TAG.sigstore.json"

[[ -f "$MANIFEST" ]] || { echo "error: $MANIFEST missing" >&2; exit 1; }
mkdir -p "$BUNDLE_DIR"

# An anchor is a statement about a specific byte string at a specific time.
# Re-signing the same version later would produce a second, later timestamp for
# the same claim, which is at best confusing and at worst reads as backdating.
[[ -e "$BUNDLE" ]] && { echo "error: $BUNDLE already exists; a sealed version is anchored once" >&2; exit 1; }

echo "signing $MANIFEST"
cosign sign-blob \
  --yes \
  --new-bundle-format \
  --bundle "$BUNDLE" \
  "$MANIFEST"

# Verify what was just produced, immediately and with the pinned identity. A
# bundle that does not verify must not be committed: an unverifiable anchor in
# the repository is worse than no anchor, because it looks like one.
echo "verifying the bundle just written"
cosign verify-blob \
  --bundle "$BUNDLE" \
  --new-bundle-format \
  --certificate-identity "$ANCHOR_IDENTITY" \
  --certificate-oidc-issuer "$ANCHOR_ISSUER" \
  "$MANIFEST"

echo "anchored $TAG -> $BUNDLE"
