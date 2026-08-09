#!/usr/bin/env bash
# The pinned Sigstore verification identity, in one place so the signing
# workflow and the verifier cannot drift apart.
#
# These two values are the whole point of keyless anchoring. Verifying a bundle
# without pinning them proves only that *somebody* signed the blob and Sigstore
# logged it — which is true of any attacker with a GitHub account. Pinning them
# says: this manifest was signed by an OIDC identity that GitHub Actions only
# issues to this workflow, in this repository, on this ref.
#
# A change here is a change to what the archive's anchors mean. It must be a
# deliberate, reviewed commit — never an incidental edit.

ANCHOR_IDENTITY="${ANCHOR_IDENTITY:-https://github.com/OlympusLedgerOrg/1f916-archive/.github/workflows/capture.yml@refs/heads/main}"
ANCHOR_ISSUER="${ANCHOR_ISSUER:-https://token.actions.githubusercontent.com}"

export ANCHOR_IDENTITY ANCHOR_ISSUER
