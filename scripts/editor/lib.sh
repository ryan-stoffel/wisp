# Sourced by the scripts in scripts/editor/.

root=$(cd "$(dirname "$0")/../.." && pwd)
pin_file="$root/editor/upstream.json"
patch_dir="$root/editor/patches"
tree="$root/editor/vscode"

fail() {
  printf '%s: %s\n' "${0##*/}" "$1" >&2
  exit 1
}

# Ignores global and system git config, so settings such as core.autocrlf,
# apply.whitespace, diff.algorithm, or format.* cannot change applied or
# exported patches. Network commands and rebases use plain git instead.
tree_git() {
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$tree" "$@"
}

read_pin() {
  fields=$(node -e '
    const pin = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const valid = /^\S+$/.test(pin.repository ?? "")
      && /^[\w.-]+$/.test(pin.tag ?? "")
      && /^[0-9a-f]{40}$/.test(pin.commit ?? "");
    if (!valid) {
      console.error("expected repository, tag, and a 40-character commit");
      process.exit(1);
    }
    console.log([pin.repository, pin.tag, pin.commit].join("\n"));
  ' "$pin_file") || fail "cannot read the pin in $pin_file"
  repository=$(printf '%s\n' "$fields" | sed -n 1p)
  tag=$(printf '%s\n' "$fields" | sed -n 2p)
  commit=$(printf '%s\n' "$fields" | sed -n 3p)
}

write_pin() {
  node -e '
    const fs = require("fs");
    const [file, tag, commit] = process.argv.slice(1);
    const pin = JSON.parse(fs.readFileSync(file, "utf8"));
    fs.writeFileSync(file, JSON.stringify({ ...pin, tag, commit }, null, 2) + "\n");
  ' "$pin_file" "$1" "$2"
}

operation_in_progress() {
  for state in rebase-apply rebase-merge MERGE_HEAD CHERRY_PICK_HEAD; do
    [ -e "$tree/.git/$state" ] && return 0
  done
  return 1
}

uncommitted_changes() {
  git -C "$tree" status --porcelain
}

fetch_tag() {
  if [ -z "$(tree_git rev-parse -q --verify "refs/tags/$1^{commit}" || true)" ]; then
    printf 'Fetching %s from %s\n' "$1" "$repository" >&2
    git -C "$tree" fetch --quiet --depth 1 --no-tags origin "+refs/tags/$1:refs/tags/$1"
  fi
  tree_git rev-parse "refs/tags/$1^{commit}"
}
