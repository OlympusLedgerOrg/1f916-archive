# Branch rules for `main`

GitHub does not read rulesets from a repository. These files are not
configuration — they are the **record** of what the branch rules are supposed to
be, kept here so that changing them is a reviewed commit rather than an
undocumented click, and so the configuration can be restored from a known-good
state if it is ever lost or altered.

That is the same reason [`scripts/anchor-identity.sh`](../../scripts/anchor-identity.sh)
holds the pinned signing identity in one committed place: a change to it is a
change to what this archive's guarantees mean, and it should read like one.

Apply either file with **Settings → Rules → Rulesets → New ruleset → Import a
ruleset**. After changing anything in the GitHub UI, export it and update the
file here in the same pull request, or this record silently becomes fiction.

---

## `main-append-only.json` — apply this one

Blocks force pushes (`non_fast_forward`) and branch deletion (`deletion`) on the
default branch. Nothing else. It has no bypass actors and needs none.

This is the rule that protects what the repository *is*. Every manifest root is a
pure function of the packet set on disk, so a force push to `main` does not just
lose commits — it rewrites the evidence, and every root recomputes to match. That
is precisely the failure this archive exists to make detectable in someone else's
database ([`README.md`](../../README.md), "1f916 hash-chains its own events…").
An archive that publishes a witness against another party's rewritable history,
while leaving its own history rewritable, has not made the argument it thinks it
has.

It cannot break the collector: `capture.yml` only ever fast-forwards `main`.

## `main-pr-gate.json` — ships disabled, and must stay that way until a bypass exists

Requires a pull request and green `rust`, `shell` and `verify` before `main`
moves.

**Enabling this without a bypass actor stops the archive collecting.** The
collector pushes directly to `main` on every hourly run
([`capture.yml`](../workflows/capture.yml), the final step), and a pull-request
rule blocks exactly that. The file is committed with `"enforcement": "disabled"`
and an empty `bypass_actors` so that importing it can never be the thing that
breaks capture. To turn it on:

1. Import it (arrives disabled, enforcing nothing).
2. Edit the ruleset → **Bypass list** → add **GitHub Actions**.
3. Confirm the bypass is listed, then set enforcement to **Active**.
4. Watch the next scheduled run still commit.

`bypass_actors` is left empty here rather than carrying a hard-coded app id,
because a wrong id produces a ruleset that looks correct and silently stops
collection. The UI resolves the actor; this file does not guess.

### Why these settings and not the obvious ones

| setting | value | why |
|---|---|---|
| `required_approving_review_count` | `0` | One maintainer. A required approval nobody can give means routinely bypassing your own rule; the PR and the green checks are the value, not the approval. |
| `strict_required_status_checks_policy` | `false` | This is "require branches to be up to date". `main` moves hourly, so enabling it means every open pull request needs a rebase about once an hour to stay mergeable. |
| required contexts | `rust`, `shell`, `verify` | The job names in [`ci.yml`](../workflows/ci.yml). `verify` is the load-bearing one — it is the same script an outside verifier runs, so a change that quietly breaks verifiability fails here rather than in someone else's terminal. |
| Socket Security checks | not required | Third-party. If the integration is removed or throttled those contexts never report, and a required context that can never report blocks every merge with no way to satisfy it. |

### Deliberately absent

- **Require signed commits.** The collector commits unsigned under a bot
  identity, so this breaks capture unless bypassed — and it buys little, because
  this archive's integrity rests on Sigstore anchors verified against a pinned
  workflow identity, not on commit signatures.
- **Require linear history.** Pull requests here land as merge commits.
