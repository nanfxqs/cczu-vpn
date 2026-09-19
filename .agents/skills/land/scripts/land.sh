#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'land: %s\n' "$*" >&2
  exit 1
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
  die "not inside a Git repository"
cd "$repo_root"

origin_url="$(git remote get-url origin 2>/dev/null)" ||
  die "origin remote is not configured"
origin_ssh_url="git@github.com:nanfxqs/cczu-vpn.git"
case "$origin_url" in
  https://github.com/nanfxqs/cczu-vpn.git | "$origin_ssh_url")
    ;;
  *)
    die "origin points to unexpected repository: $origin_url"
    ;;
esac
if [[ "$origin_url" != "$origin_ssh_url" ]]; then
  git remote set-url origin "$origin_ssh_url"
fi

current_branch="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
  die "HEAD is detached"
[[ "$current_branch" == "main" ]] ||
  die "landing must run from main, currently on $current_branch"

command="${1:-}"
case "$command" in
  preflight)
    git --no-optional-locks status --short --branch
    git remote -v
    ;;

  verify)
    # Formatting/check commands: README.zh.md, “常用命令”.
    cargo fmt -- --check
    cargo check --locked

    # Test targets are defined by Cargo.toml and #[test] modules under src/.
    cargo test --locked

    # Exact release build is documented in README.zh.md and both workflows.
    cargo build --release --locked
    git diff --check
    ;;

  sync)
    [[ -z "$(git status --porcelain)" ]] ||
      die "commit or restore all worktree changes before synchronization"
    git fetch origin main
    if ! git merge-base --is-ancestor origin/main HEAD; then
      git rebase origin/main
    fi
    ;;

  publish)
    [[ -z "$(git status --porcelain)" ]] ||
      die "worktree must be clean before publication"
    git fetch origin main
    git merge-base --is-ancestor origin/main HEAD ||
      die "HEAD is not based on the latest origin/main; run sync and verify again"
    local_sha="$(git rev-parse HEAD)"
    git push origin HEAD:main
    remote_sha="$(
      git ls-remote origin refs/heads/main |
        awk 'NR == 1 { print $1 }'
    )"
    [[ -n "$remote_sha" ]] || die "could not read origin/main after push"
    [[ "$remote_sha" == "$local_sha" ]] ||
      die "origin/main is $remote_sha, expected $local_sha"
    printf 'landed %s on origin/main\n' "$local_sha"
    ;;

  *)
    die "usage: $0 {preflight|verify|sync|publish}"
    ;;
esac
