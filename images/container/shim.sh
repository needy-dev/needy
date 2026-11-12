#!/bin/sh
# Build-time shims, the last step of a container actor image (see
# docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md,
# "Exec routing with build-time shims", and
# docs/ai/references/architecture/2026-11-12-actor-images-design.md).
#
# Every executable in the given PATH directories is copied to
# /.needy/real/<its path> and replaced by a hard link to the proxy (the shim
# stub, images/stub/shim.c), which hands the exec to the guest needy with the
# path of the link it was started through; the guest needy runs the real
# program with the caller's argv (argv[0] included), and asks the daemon only
# for the names a run routes. A link to a program in another directory
# becomes a shim too, whose real program is a link to that program; a link to
# a program in a shimmed directory stays a link, since its target is a shim.
#
# The base image keeps this script as /.needy/shim-image. An image `FROM` the
# base runs it as its last step (`RUN ["/.needy/shim-image"]`), so that the
# programs it added are shims too. It may run any number of times: a program
# that is a shim already is left as it is, and one that replaced a shim (a
# package upgrade) becomes the new real program.
#
# Usage: shim.sh [ROOT PROXY DIR...]
#   ROOT   the image's root: empty in the image, a directory in tests
#   PROXY  the proxy's path in the image
#   DIR    the directories on the image's PATH
# Without arguments: the image's own root, /.needy/proxy and Docker's
# default container PATH.
#
# While no guest needy is injected (an image build), a shim runs its real
# program unrouted. A tool this script replaces has no real program until
# the script copied it, so the script first copies aside the tools it uses
# that it is about to replace, and runs those.
#
# Not covered: programs outside these directories run by their full path,
# and programs created after the image was built (see the design note).
set -eu
# NOTICE: the default directories are Docker's default container PATH,
# https://github.com/moby/moby/blob/v28.5.1/oci/defaults.go#L19 (as
# `SYSTEM_PATH` in crates/core-actor/src/guest/init/stubs.rs).
if [ $# -eq 0 ]; then
  set -- "" /.needy/proxy /usr/local/sbin /usr/local/bin /usr/sbin /usr/bin /sbin /bin
fi
if [ -n "$1" ]; then
  root=$(cd "$1" && pwd -P)
else
  root=
fi
proxy=$2
shift 2
real=$root/.needy/real

# The directories to shim, by their physical paths, once each: /bin and
# /sbin are links to usr/bin and usr/sbin on merged-/usr systems.
dirs=
for dir in "$@"; do
  [ -d "$root$dir" ] || continue
  physical=$(cd "$root$dir" && pwd -P)
  case " $dirs " in
    *" $physical "*) continue ;;
  esac
  dirs="$dirs $physical"
done

# Whether the file $1 is directly in a shimmed directory.
shimmed() {
  case " $dirs " in
    *" $(dirname "$1") "*) return 0 ;;
  esac
  return 1
}

# Whether the file $1 is a shim: a hard link to the proxy, or a copy of it.
# NOTICE: `-ef` alone is not enough in an image `FROM` the base. Its
# shims are hard links in a lower layer, and the first `ln` in the new
# layer copies the proxy up, so the shims and the proxy no longer share an
# inode; seen with BuildKit on overlayfs (Docker 29.8, 2026-11-13), where a
# second run took the shims for real programs.
is_proxy() {
  [ "$1" -ef "$root$proxy" ] || cmp -s "$1" "$root$proxy"
}

# The tools below, copied aside where they are about to become shims; a
# tool that is a shim already (a second run) is copied from its real
# program. In tests, whose root is a directory, the host's tools are never
# shimmed, so nothing is copied (macOS kills a copied system binary anyway).
aside=$(mktemp -d)
for tool in readlink dirname mkdir ln cp rm cmp; do
  found=$(command -v "$tool")
  physical="$(cd "${found%/*}" && pwd -P)/$tool"
  if is_proxy "$physical"; then
    cp "$real${physical#"$root"}" "$aside/$tool"
  elif shimmed "$physical"; then
    cp "$found" "$aside/$tool"
  fi
done
PATH=$aside:$PATH
trap 'rm -rf "$aside"' EXIT

for dir in $dirs; do
  for file in "$dir"/*; do
    path=${file#"$root"}
    # A shim already: its real program is in place.
    if [ ! -L "$file" ] && [ -f "$file" ] && is_proxy "$file"; then
      continue
    fi
    if [ -L "$file" ]; then
      target=$(readlink -f "$file") || continue
      if shimmed "$target" || [ ! -f "$target" ] || [ ! -x "$target" ]; then
        continue
      fi
      mkdir -p "$real$(dirname "$path")"
      rm -f "$real$path"
      ln -s "${target#"$root"}" "$real$path"
      ln -f "$root$proxy" "$file"
    elif [ -f "$file" ] && [ -x "$file" ]; then
      mkdir -p "$real$(dirname "$path")"
      # Replaced, not written through: a real program kept by an earlier
      # run may be a link.
      rm -f "$real$path"
      cp -a "$file" "$real$path"
      ln -f "$root$proxy" "$file"
    fi
  done
done
