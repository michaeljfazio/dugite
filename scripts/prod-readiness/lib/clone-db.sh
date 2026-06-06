#!/usr/bin/env bash
# clone-db.sh <src-db-dir> <clone-name>  — APFS copy-on-write clone, disk-fit-guarded.
# Prints the clone path on stdout. The source db is never touched (cp -Rc is CoW).
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"
here="$(dirname "${BASH_SOURCE[0]}")"

src="${1:?src db dir}"; name="${2:?clone name}"
[ -d "$src" ] || die "src db not found: $src"

size_gb=$(du -sg "$src" | awk '{print $1}')
if ! "$here/gc-disk.sh" fits "$size_gb"; then
  log "clone of ${size_gb}GB would breach MIN_FREE_GB=$MIN_FREE_GB; GC then retry"
  net="${name%%-*}"
  "$here/gc-disk.sh" gc-clones "$net" || true
  "$here/gc-disk.sh" fits "$size_gb" || die "still no room for a ${size_gb}GB clone"
fi

dest="$CLONES_DIR/$name"
rm -rf "$dest"
cp -Rc "$src" "$dest"      # -c = APFS clone (copy-on-write); src untouched
log "cloned $src -> $dest (${size_gb}GB logical)"
echo "$dest"
