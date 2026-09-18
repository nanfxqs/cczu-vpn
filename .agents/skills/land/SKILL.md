---
name: land
description: >-
  Land changes in the cczu-vpn repository after the user explicitly requests
  landing. Use only for Land Changes or an equivalent explicit merge request,
  not for review, preparation, verification alone, or skill installation.
metadata:
  delta-action: land
---

# Land changes

An invocation of this skill is the landing authorization. Proceed without
asking for the same permission again.

The repository publishes directly to `origin/main`. The `local` remote is a
backlink to the user's primary checkout and is never a publication target.
Use `.agents/skills/land/scripts/land.sh` for deterministic preflight,
verification, synchronization, and publication. Keep scope selection, review,
commit messages, and conflict judgment with the agent.

## 1. Establish the change

Run:

```sh
bash .agents/skills/land/scripts/land.sh preflight
git status --short
git diff --check
git diff
```

Account for every tracked and untracked change. Include only the requested
change and files necessary for it. Preserve unrelated user work. Stop and ask
one focused question when ownership or scope cannot be established safely.

Review the complete landing diff for correctness, secrets, generated debris,
and consistency with `README.md`, `README.zh.md`, and the implementation.

## 2. Verify

Run:

```sh
bash .agents/skills/land/scripts/land.sh verify
```

This executes the repository's Rust formatting, tests, locked dependency
check, and release build. The command sources are `README.zh.md` under
“常用命令”, the test targets declared in `src`, `Cargo.toml`, and the release
build invocations in `.github/workflows/nightly.yml` and
`.github/workflows/release.yml`.

Resolve failures in scope and repeat `verify` until it passes. A skipped,
pending, stale, or partially successful verification is a blocker.

## 3. Commit the reviewed scope

Stage explicit paths rather than using `git add -A`. Confirm the staged diff
contains the complete requested change and no unrelated work:

```sh
git diff --cached --check
git diff --cached
```

Create one focused commit with a concise message:

```sh
GIT_EDITOR=true git commit -m "<message>"
```

Do not create release tags. This repository's release workflow is triggered
by `v*` tags (`.github/workflows/release.yml`); landing to `main` is not a
release request.

## 4. Synchronize with main

Run:

```sh
bash .agents/skills/land/scripts/land.sh sync
```

The script fetches `origin/main` and rebases the landing commit when needed.
Resolve conflicts automatically when the intended result is clear, stage the
resolution, and continue non-interactively with:

```sh
GIT_EDITOR=true git rebase --continue
```

For ambiguous conflicts or resolutions that could discard unrelated work,
abort the rebase and ask the user. After any rebase or conflict resolution,
rerun:

```sh
bash .agents/skills/land/scripts/land.sh verify
```

The final commit—not an earlier revision—must pass.

## 5. Publish and prove landing

Run:

```sh
bash .agents/skills/land/scripts/land.sh publish
```

The script pushes `HEAD` to `origin/main` without force and verifies that the
remote SHA exactly matches the local commit. Only that verified match is a
successful landing. A local commit, passing build, topic-branch push, or
started workflow is not success.

If publication is rejected, preserve the commit, report that it has not
landed, and explain the concrete blocker. Never force-push or weaken checks.

## 6. Report the outcome

Record the short SHA and, when available, verified GitHub commit and CI URLs.
This repository has no push-to-main CI workflow; do not invent a CI link.

When running in a subthread and `report_subthread_status` is available, report
the final outcome to the parent:

- Verified remote SHA: `status: "success"`, title `Landed on main`, and a
  one-line description linking the commit.
- Failed checks, ambiguous conflicts, or rejected publication:
  `status: "failure"` with a short blocker description ending in `Not landed.`

Otherwise report the same result directly in the current conversation.
Report success only after `publish` proves `origin/main` equals the landed
commit.
