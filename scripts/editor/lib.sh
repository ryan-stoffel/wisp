# Sourced by the scripts in scripts/editor/.

root=$(cd "$(dirname "$0")/../.." && pwd)
pin_file="$root/editor/upstream.json"
patch_dir="$root/editor/patches"
product_patch="$root/editor/product.json"
overlay_dir="$root/editor/overlay"
tree="$root/editor/vscode"
prepare_name='wisp prepare'
prepare_email='prepare@wisp.invalid'

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

# Lists the files under editor/overlay relative to it, sorted, one per line.
# Skips Finder's .DS_Store files.
overlay_files() {
  [ -d "$overlay_dir" ] || return 0
  odd=$(cd "$overlay_dir" && find . ! -type f ! -type d | sed -n 1p)
  [ -z "$odd" ] || fail "editor/overlay/${odd#./} is not a regular file"
  listed=$(cd "$overlay_dir" && find . -type f ! -name .DS_Store | sed 's|^\./||' | LC_ALL=C sort)
  if printf '%s\n' "$listed" | grep -Eq '^(product\.json|\.git(/|$))'; then
    fail "editor/overlay cannot hold product.json or .git; change product.json with editor/product.json"
  fi
  printf '%s\n' "$listed" | sed '/^$/d'
}

# Hashes everything that decides the prepared tree: the pin, the patches, the
# overlay, and these scripts.
inputs_stamp() {
  stamp_files=$(overlay_files) || exit 1
  {
    printf '%s\n' "$repository" "$tag" "$commit"
    for input in "$patch_dir"/*.patch "$product_patch" "$(dirname "$0")"/*; do
      [ -f "$input" ] || continue
      printf '%s\n' "${input#"$root"/}"
      cat "$input"
    done
    printf '%s\n' "$stamp_files" | while IFS= read -r file; do
      [ -n "$file" ] || continue
      if [ -x "$overlay_dir/$file" ]; then mode=755; else mode=644; fi
      printf 'editor/overlay/%s %s\n' "$file" "$mode"
      cat "$overlay_dir/$file"
    done
  } | git hash-object --stdin
}

# Writes upstream's product.json at commit $1 to file $2, with editor/product.json
# merged in as a JSON merge patch (RFC 7396): objects merge key by key, null
# deletes a key, and any other value replaces the old one. Keys keep their order.
merge_product() {
  tree_git show "$1:product.json" >"$2.upstream" || fail "$1 has no product.json"
  node -e '
    const fs = require("fs");
    const [upstreamFile, patchFile, outFile] = process.argv.slice(1);
    const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
    const merge = (target, patch) => {
      if (!isObject(patch)) {
        return patch;
      }
      const result = isObject(target) ? { ...target } : {};
      for (const [key, value] of Object.entries(patch)) {
        if (value === null) {
          delete result[key];
        } else {
          result[key] = merge(result[key], value);
        }
      }
      return result;
    };
    const patch = JSON.parse(fs.readFileSync(patchFile, "utf8"));
    if (!isObject(patch)) {
      console.error(patchFile + " must hold a JSON object");
      process.exit(1);
    }
    const merged = merge(JSON.parse(fs.readFileSync(upstreamFile, "utf8")), patch);
    fs.writeFileSync(outFile, JSON.stringify(merged, null, "\t") + "\n");
  ' "$2.upstream" "$product_patch" "$2" || fail "cannot merge editor/product.json into product.json"
}

# Prints the commit that puts wisp's overlay on upstream commit $1:
# editor/product.json merged into product.json, and each file under
# editor/overlay copied to the same path. The patches apply on top of it.
# Its author, committer, and date are fixed ($1's date), so the same upstream
# commit and overlay give the same commit on any machine. Without an overlay,
# prints $1.
make_base() {
  base_files=$(overlay_files) || exit 1
  if [ ! -e "$product_patch" ] && [ -z "$base_files" ]; then
    printf '%s\n' "$1"
    return 0
  fi
  scratch="$tree/.git/wisp-base"
  rm -rf "$scratch"
  mkdir -p "$scratch"
  GIT_INDEX_FILE="$scratch/index" tree_git read-tree "$1"
  if [ -e "$product_patch" ]; then
    merge_product "$1" "$scratch/product.json"
    blob=$(tree_git hash-object -w --no-filters "$scratch/product.json")
    GIT_INDEX_FILE="$scratch/index" tree_git update-index --add --cacheinfo "100644,$blob,product.json"
  fi
  printf '%s\n' "$base_files" | while IFS= read -r file; do
    [ -n "$file" ] || continue
    if [ -x "$overlay_dir/$file" ]; then mode=100755; else mode=100644; fi
    blob=$(tree_git hash-object -w --no-filters "$overlay_dir/$file")
    GIT_INDEX_FILE="$scratch/index" tree_git update-index --add --cacheinfo "$mode,$blob,$file"
  done
  base_tree=$(GIT_INDEX_FILE="$scratch/index" tree_git write-tree)
  rm -rf "$scratch"
  date=$(tree_git show -s --format=%cI "$1")
  GIT_AUTHOR_NAME=$prepare_name GIT_AUTHOR_EMAIL=$prepare_email GIT_AUTHOR_DATE=$date \
    GIT_COMMITTER_NAME=$prepare_name GIT_COMMITTER_EMAIL=$prepare_email GIT_COMMITTER_DATE=$date \
    tree_git commit-tree "$base_tree" -p "$1" \
    -m 'wisp: overlay editor/product.json and editor/overlay' \
    -m 'scripts/editor/prepare generates this commit, and export-patches never exports it. See docs/decisions/0008-editor-overlay.md in wisp.'
}

# Prints the commit the patches in the tree sit on: refs/wisp/base, or the
# pinned commit for a tree prepared before overlays existed.
current_base() {
  tree_git rev-parse -q --verify refs/wisp/base || printf '%s\n' "$commit"
}
