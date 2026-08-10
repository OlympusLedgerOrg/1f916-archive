#!/usr/bin/env bash
# Commit a capture to `main` as a *signed* commit, without holding a signing key.
#
# `git commit && git push` from a workflow produces an unsigned commit. Requiring
# verified signatures on `main` therefore rejects every capture, and the usual fix
# — a GPG private key in Actions secrets — would introduce exactly the long-lived
# key that README.md says this project does not have.
#
# So the commit is created server-side instead, through the GraphQL
# `createCommitOnBranch` mutation. GitHub builds the commit and signs it with its
# own key, so it verifies; the only credential involved is the run's ephemeral
# `GITHUB_TOKEN`. No key is stored anywhere, and the claim in README.md stays true.
#
# This changes nothing about what the archive proves. Commit signatures are not
# what makes a packet evidence — the manifest root, its Sigstore bundle, and the
# pinned workflow identity are, and they are unaffected. A verified commit is a
# smaller, separate guarantee: that this history was written by this workflow.
#
# TRADE-OFF, RECORDED RATHER THAN HIDDEN: `createCommitOnBranch` has no author
# field. The commit author and committer become the Actions identity, so the
# "1f916-archive collector" name no longer appears in `git log --format=%an`. The
# provenance moves into the commit body, which is the honest place for it once the
# author field is no longer ours to set.
#
# Usage: scripts/commit-capture.sh "<commit headline>"
#
# Requires: gh, jq, and GH_TOKEN in the environment.

set -euo pipefail

HEADLINE="${1:?usage: commit-capture.sh <headline>}"
REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}"
BRANCH="${CAPTURE_BRANCH:-main}"

# A capture is ~100 files and ~2 MB, so ~2.9 MB once base64-encoded. This ceiling
# is far above that and far below anything the API would refuse: it exists to turn
# a runaway capture into a loud failure rather than a rejected request whose cause
# has to be guessed at from a transport error.
MAX_PAYLOAD_BYTES="${MAX_PAYLOAD_BYTES:-41943040}"

need() { command -v "$1" >/dev/null || { echo "commit-capture: missing dependency: $1" >&2; exit 1; }; }
need gh
need jq

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Stage only the three evidence trees, exactly as the previous `git add` did.
# Staging is how the change set is computed; nothing is committed locally.
git add -A archive artifacts state
if git diff --cached --quiet; then
  echo "nothing captured this run"
  exit 0
fi

: >"$work/additions.ndjson"
: >"$work/deletions.ndjson"

# `-z` keeps paths intact even if one ever contained whitespace. packet.rs builds
# every filename from a compile-time constant or a locally formatted integer, so
# that cannot currently happen — but this loop should not be the thing that breaks
# if that ever changes.
# Encode one file as a FileChanges addition.
#
# The base64 goes to a temp file and reaches jq via `--rawfile`, never `--arg`.
# A packet is capped at 16 MB by src/http.rs, and a `/api/changes` page carrying
# 200 posts and 500 comments is already hundreds of kilobytes — passing that
# through argv exceeds ARG_MAX and fails with "Argument list too long". `--rawfile`
# reads from disk and has no such limit.
#
# `base64 -w0` emits no line breaks; `rtrimstr` removes the trailing newline if a
# future coreutils adds one, so the encoded string is exactly the payload.
add_file() {
  local path="$1"
  base64 -w0 "$path" >"$work/b64"
  jq -nc --arg p "$path" --rawfile c "$work/b64" \
    '{path: $p, contents: ($c | rtrimstr("\n"))}' >>"$work/additions.ndjson"
}

encode() {
  local status path newpath
  while IFS= read -r -d '' status && IFS= read -r -d '' path; do
    case "$status" in
      D)
        jq -nc --arg p "$path" '{path: $p}' >>"$work/deletions.ndjson"
        ;;
      R*)
        # A rename arrives as old path then new path: drop the old, add the new.
        IFS= read -r -d '' newpath
        jq -nc --arg p "$path" '{path: $p}' >>"$work/deletions.ndjson"
        add_file "$newpath"
        ;;
      *)
        add_file "$path"
        ;;
    esac
  done
}
git diff --cached --name-status -z | encode

jq -s . "$work/additions.ndjson" >"$work/additions.json"
jq -s . "$work/deletions.ndjson" >"$work/deletions.json"

adds=$(jq 'length' "$work/additions.json")
dels=$(jq 'length' "$work/deletions.json")
payload_bytes=$(jq '[.[].contents | length] | add // 0' "$work/additions.json")
echo "commit-capture: ${adds} additions, ${dels} deletions, ${payload_bytes} encoded bytes" >&2

if [ "$payload_bytes" -gt "$MAX_PAYLOAD_BYTES" ]; then
  echo "commit-capture: encoded payload ${payload_bytes} exceeds the ${MAX_PAYLOAD_BYTES}-byte ceiling; refusing to attempt the mutation" >&2
  exit 1
fi

# The commit this run was built on. Passing it as `expectedHeadOid` makes the
# write a compare-and-swap: if anything else advanced the branch since checkout,
# the mutation fails instead of silently committing packets against a tree that
# moved. The concurrency group already serialises capture runs; this makes the
# guarantee explicit rather than assumed.
head_oid="$(git rev-parse HEAD)"

read -r -d '' QUERY <<'GRAPHQL' || true
mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) {
    commit { oid url }
  }
}
GRAPHQL

BODY="Captured and sealed by the 1f916-archive collector (.github/workflows/capture.yml).

Created through createCommitOnBranch so GitHub signs it; the workflow holds no
signing key. The commit author is the Actions identity because the mutation has
no author field."

jq -n \
  --arg query "$QUERY" \
  --arg repo "$REPO" \
  --arg branch "$BRANCH" \
  --arg oid "$head_oid" \
  --arg headline "$HEADLINE" \
  --arg body "$BODY" \
  --slurpfile additions "$work/additions.json" \
  --slurpfile deletions "$work/deletions.json" \
  '{
     query: $query,
     variables: {
       input: {
         branch: {
           repositoryNameWithOwner: $repo,
           branchName: $branch
         },
         expectedHeadOid: $oid,
         message: { headline: $headline, body: $body },
         fileChanges: {
           additions: $additions[0],
           deletions: $deletions[0]
         }
       }
     }
   }' >"$work/payload.json"

# `gh api graphql` surfaces GraphQL-level errors as a non-zero exit, so a rule
# violation or a moved branch fails the run rather than being reported as success.
gh api graphql --input "$work/payload.json" >"$work/response.json"

oid="$(jq -r '.data.createCommitOnBranch.commit.oid // empty' "$work/response.json")"
url="$(jq -r '.data.createCommitOnBranch.commit.url // empty' "$work/response.json")"
if [ -z "$oid" ]; then
  echo "commit-capture: mutation returned no commit oid" >&2
  jq . "$work/response.json" >&2 || cat "$work/response.json" >&2
  exit 1
fi

echo "commit-capture: committed ${oid}"
[ -n "$url" ] && echo "commit-capture: ${url}"
