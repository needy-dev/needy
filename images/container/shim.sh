#!/bin/sh
# Build-time shims, the last step of the container actor image (see
# docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md,
# "Exec routing with build-time shims").
#
# Every executable in the given PATH directories is copied to
# /.needy/real/<its path> and replaced by a hard link to the proxy, which
# finds the program from the link it was started through, runs the real one
# with the caller's argv (argv[0] included), and asks the daemon only for the
# names a run routes. A link to a program in another directory becomes a shim
# too, whose real program is a link to that program; a link to a program in a
# shimmed directory stays a link, since its target is a shim.
#
# Usage: shim.sh ROOT PROXY DIR...
#   ROOT   the image's root: empty in the image, a directory in tests
#   PROXY  the proxy's path in the image
#   DIR    the directories on the image's PATH
#
# Once a program is replaced, running it goes through the proxy, which runs
# its real program while routing is off (no /run/needy/route), so this
# script keeps working while it replaces the tools it uses.
#
# Not covered: programs outside these directories run by their full path,
# and programs created after the image was built (see the design note).
set -eu

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

for dir in $dirs; do
  for file in "$dir"/*; do
    path=${file#"$root"}
    if [ -L "$file" ]; then
      target=$(readlink -f "$file") || continue
      if shimmed "$target" || [ ! -f "$target" ] || [ ! -x "$target" ]; then
        continue
      fi
      mkdir -p "$real$(dirname "$path")"
      ln -s "${target#"$root"}" "$real$path"
      ln -f "$root$proxy" "$file"
    elif [ -f "$file" ] && [ -x "$file" ]; then
      mkdir -p "$real$(dirname "$path")"
      cp -a "$file" "$real$path"
      ln -f "$root$proxy" "$file"
    fi
  done
done
