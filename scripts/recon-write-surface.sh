#!/usr/bin/env bash
# Read-only reconnaissance of the 1f916.ai *write* surface.
#
# The archive has never written to 1f916 (docs/api-semantics.md, Method). Before
# that changes, the write surface has to be measured the same way the read
# surface was: by probing it, not by assuming it. This script is that probe.
#
# READ-ONLY, MECHANICALLY. Exactly two methods are reachable from here — GET and
# OPTIONS — and they are constants in `probe()`, not values a caller can supply.
# Both are `safe` per RFC 9110 section 9.2.1: they create no upstream state.
# Nothing in this script can POST, and no candidate path list can make it.
#
# OPTIONS is the point of the exercise. It reports which methods an endpoint
# implements *without attempting them*, so the write surface can be mapped
# without a single write. Where OPTIONS is uninformative the recon stops and
# says so, rather than escalating to a probe write.
#
# TWO OUTPUTS, SEPARATED ON PURPOSE:
#
#   <out>/structure.json   Structural observations only: status codes, media
#                          types, Allow tokens, CORS method policy (kept
#                          separate — it is not an Allow header), auth-challenge
#                          scheme tokens, body lengths, and JSON key paths with
#                          every value stripped. Every one of those is validated
#                          against a bounded pattern and dropped rather than
#                          reproduced if it does not conform, so this file stays
#                          safe to read, quote, diff, and commit.
#
#   <out>/prose/*.body     Raw response bodies, verbatim. Upstream-controlled
#                          bytes. FOR HUMAN EYES ONLY — never summarise, quote
#                          into a prompt, or feed these to a language model,
#                          per the rule in README.md ("Untrusted content").
#
# That split is what lets the recon be written up honestly. Everything the
# report asserts comes from structure.json; anything that needed a human to read
# prose is marked as such in the report and attributed to a person.
#
# Usage: scripts/recon-write-surface.sh [output-dir]

set -euo pipefail

# Pin the locale. Every `tr`, `grep` and `sort` below runs over upstream-controlled
# bytes, and their case-folding and collation are locale-dependent. HTTP tokens are
# ASCII (RFC 9110 §5.6.2); parsing them under whatever locale the runner happens to
# have is a way to get different findings from the same response.
export LC_ALL=C

OUT="${1:-recon-out}"
BASE="${F916_BASE:-https://1f916.ai}"

# Matches the collector's posture: identify the project, carry a contact URL, and
# leave at least this long between requests. We are a guest here.
UA="1f916-archive-recon/0.1 (read-only write-surface reconnaissance; contact: https://github.com/OlympusLedgerOrg/1f916-archive)"
MIN_INTERVAL="${F916_MIN_INTERVAL:-0.6}"
MAX_TIME="${F916_MAX_TIME:-30}"
# Recon needs shapes, not corpora. A body past this cap is recorded as oversized
# rather than pulled down.
MAX_BYTES="${F916_MAX_BYTES:-1048576}"

# Candidate paths. Four groups, and the first exists to prove the probe works: if
# the known-live controls do not come back 200, nothing else here means anything
# and the report must not be written.
CONTROLS=(/api/attest /api/official /api/docket)
DISCOVERY=(/ /llms.txt /robots.txt /openapi.json /api /api/docs /api/openapi.json /.well-known/security.txt)
IDENTITY=(/api/register /api/signup /api/join /api/apply /api/citizen /api/citizens /api/me /api/session /api/auth /api/token)
WRITES=(/api/post /api/posts /api/comment /api/comments /api/vote /api/flag)

need() { command -v "$1" >/dev/null || { echo "recon: missing dependency: $1" >&2; exit 1; }; }
need curl
need jq

mkdir -p "$OUT/prose"
: >"$OUT/observations.ndjson"

slug() { printf '%s' "${1//\//_}" | sed 's/^_//; s/^$/root/; s/[^A-Za-z0-9._-]/_/g'; }

# Normalise a raw header block: strip CR, lowercase field names, emit
# "name<TAB>value". Done once per response so the extractors below are plain
# field matches. `IGNORECASE` would be simpler and is a gawk extension that
# silently does nothing under mawk, which is what CI runners ship.
normalise_headers() {
  tr -d '\r' <"$1" | awk '
    /^[A-Za-z][A-Za-z0-9-]*:/ {
      i = index($0, ":")
      name = tolower(substr($0, 1, i - 1))
      val = substr($0, i + 1)
      sub(/^[ \t]+/, "", val)
      print name "\t" val
    }'
}

header_first() { awk -F'\t' -v k="$1" '$1 == k { print $2; exit }' "$2"; }
header_all() { awk -F'\t' -v k="$1" '$1 == k { print $2 }' "$2"; }

# Emit the JSON *shape* of a body: every key path, with all values discarded.
#
# Key names are themselves upstream-controlled text, so they are filtered to a
# conservative character class and length. A key that does not conform is
# counted, never reproduced — the count is the finding, and a human can read the
# raw body in prose/ to see what it was.
shape() {
  jq -c '
    def norm: map(if type == "number" then "[]" else . end) | join(".");
    def ok: test("^[A-Za-z0-9_.\\[\\]-]{1,40}$");
    [paths(scalars) | norm] | unique
    | { keys: map(select(ok)), nonconforming_keys: map(select(ok | not)) | length }
  ' <"$1" 2>/dev/null || printf '{"keys":null,"nonconforming_keys":null}'
}

# One safe request. $2 is constrained to the two constants below; any other value
# is a programming error and aborts the run rather than being sent.
probe() {
  local path="$1" method="$2"
  case "$method" in
    GET | OPTIONS) ;;
    *)
      echo "recon: refusing method '$method' — this script cannot write" >&2
      exit 1
      ;;
  esac

  sleep "$MIN_INTERVAL"

  local s hdr body url curl_status=0
  s="$(slug "$path")"
  hdr="$OUT/prose/${s}.${method}.hdr"
  body="$OUT/prose/${s}.${method}.body"
  url="${BASE}${path}"

  # `-q` must be the *first* argument: it is what stops curl reading a default
  # config, and a `~/.curlrc` carrying `-X POST` or `-d` would silently turn this
  # script's central guarantee into a false statement. `-X` is then set
  # explicitly on both branches so the method on the wire is the method in the
  # case label rather than a curl default.
  #
  # `--max-filesize` bounds the transfer itself instead of trusting the peer to
  # stop, which is the same protection `src/http.rs` gives the collector. Since
  # curl 8.4.0 it also aborts a chunked response whose length was never declared;
  # under an older curl it only catches a declared Content-Length, and the
  # `--max-time` ceiling is the remaining backstop.
  local capped=false
  case "$method" in
    GET) curl -q -sS --max-time "$MAX_TIME" --max-filesize "$MAX_BYTES" -A "$UA" -X GET -D "$hdr" -o "$body" "$url" || curl_status=$? ;;
    OPTIONS) curl -q -sS --max-time "$MAX_TIME" --max-filesize "$MAX_BYTES" -A "$UA" -X OPTIONS -D "$hdr" -o "$body" "$url" || curl_status=$? ;;
  esac

  # 63 is CURLE_FILESIZE_EXCEEDED. The response envelope arrived, so status and
  # headers are real observations; only the body is a prefix. That is a finding,
  # not a transport failure — but the byte count on disk is then the cap rather
  # than a measurement, so it is reported as unknown rather than as a length.
  if [ "$curl_status" -eq 63 ]; then
    capped=true
    curl_status=0
  fi

  if [ "$curl_status" -ne 0 ]; then
    jq -nc --arg p "$path" --arg m "$method" --argjson e "$curl_status" \
      '{path:$p, method:$m, transport_error:$e}' >>"$OUT/observations.ndjson"
    return 0
  fi

  local norm status media media_ok allow cors auth len shape_json
  norm="${hdr}.norm"
  normalise_headers "$hdr" >"$norm"

  status="$(awk 'toupper($0) ~ /^HTTP\// { c = $2 } END { print c + 0 }' "$hdr")"

  # Header *values* are upstream text too. Only these four are lifted out, and
  # each is reduced to bounded tokens: a media type, two method sets, and an auth
  # scheme without its realm. Every extractor ends `|| true` because a header
  # that is simply absent is an ordinary result, not a failure — and `pipefail`
  # would otherwise turn each empty grep into an aborted run.
  #
  # The media type gets the same treatment as a JSON key name: validated against
  # a conservative pattern, and dropped rather than reproduced if it does not
  # conform. `structure.json` is documented as safe to quote and gets rendered
  # into a Markdown table by the workflow, and neither claim survives copying an
  # arbitrary upstream string into it. `media_type_conforms: false` records that
  # something was served and rejected, which is itself a finding.
  media="$(header_first content-type "$norm" | sed 's/;.*//' | tr '[:upper:]' '[:lower:]' | tr -d ' ' || true)"
  media_ok=true
  if [ -n "$media" ] && ! printf '%s' "$media" | grep -qE '^[a-z0-9][a-z0-9.+-]{0,62}/[a-z0-9][a-z0-9.+-]{0,62}$'; then
    media_ok=false
    media=""
  fi

  # `Allow` and `Access-Control-Allow-Methods` are kept apart, because they are
  # different claims and only one of them answers this recon's question. `Allow`
  # (RFC 9110 §10.2.1) is the origin server stating which methods *this resource*
  # implements. A CORS method list is a browser policy, routinely emitted as a
  # blanket `GET, POST, PUT, DELETE, OPTIONS` by middleware on every route
  # regardless of what the route does. Merging them would let one careless
  # middleware default report a write surface on every path probed here — a
  # false positive in exactly the direction that would matter most.
  allow="$(header_all allow "$norm" | tr ',' '\n' | tr -d ' ' | tr '[:lower:]' '[:upper:]' | grep -E '^[A-Z]{3,7}$' | sort -u | paste -sd, - || true)"
  cors="$(header_all access-control-allow-methods "$norm" | tr ',' '\n' | tr -d ' ' | tr '[:lower:]' '[:upper:]' | grep -E '^[A-Z]{3,7}$' | sort -u | paste -sd, - || true)"
  auth="$(header_first www-authenticate "$norm" | grep -Eo '^[A-Za-z]{1,20}' || true)"

  len="$(wc -c <"$body" | tr -d ' ')"

  shape_json='null'
  if [ "$media" = "application/json" ] && [ "$len" -gt 0 ] && [ "$capped" = false ]; then
    shape_json="$(shape "$body")"
  fi

  jq -nc \
    --arg p "$path" --arg m "$method" --argjson st "${status:-0}" \
    --arg media "$media" --argjson media_ok "$media_ok" \
    --arg allow "$allow" --arg cors "$cors" --arg auth "$auth" \
    --argjson len "$len" --argjson capped "$capped" --argjson shape "$shape_json" \
    '{path:$p, method:$m, status:$st,
      media_type: ($media | if . == "" then null else . end),
      media_type_conforms: $media_ok,
      allow: ($allow | if . == "" then null else split(",") end),
      cors_allow_methods: ($cors | if . == "" then null else split(",") end),
      auth_scheme: ($auth | if . == "" then null else . end),
      body_bytes: (if $capped then null else $len end),
      body_capped: $capped,
      shape: $shape}' \
    >>"$OUT/observations.ndjson"
}

echo "recon: base=$BASE out=$OUT (GET and OPTIONS only; no writes)" >&2

for p in "${CONTROLS[@]}"; do probe "$p" GET; done

# Abort before probing anything unknown if the controls did not come back. A
# report full of 404s from a host that is simply down would read exactly like a
# report from a host with no write surface.
live="$(jq -s '[.[] | select(.status == 200)] | length' "$OUT/observations.ndjson")"
if [ "$live" -lt 1 ]; then
  echo "recon: no control endpoint returned 200 — aborting rather than publishing a misleading report" >&2
  exit 1
fi

for p in "${DISCOVERY[@]}" "${IDENTITY[@]}" "${WRITES[@]}"; do
  probe "$p" GET
  probe "$p" OPTIONS
done

jq -s --arg base "$BASE" --arg ua "$UA" \
  '{ tool: "recon-write-surface/0.1",
     base: $base,
     user_agent: $ua,
     methods_used: ["GET", "OPTIONS"],
     note: "Structural observations only. Values, prose and header text are excluded by construction; raw bodies are in prose/ and are for human eyes only.",
     observations: . }' \
  "$OUT/observations.ndjson" >"$OUT/structure.json"

echo "recon: wrote $OUT/structure.json ($(jq '.observations | length' "$OUT/structure.json") observations)" >&2
echo "recon: raw bodies in $OUT/prose/ — human eyes only, never into a model" >&2
