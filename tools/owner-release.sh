#!/bin/sh
# Build and sign one platform release of an owner's own Cobalt.
#
#   tools/owner-release.sh BUILD SEED_FILE OUT_DIR
#
# BUILD is 1 to 99: the owner's build number on top of the upstream version
# in Cargo.toml. Upstream 0.3.37 with BUILD 1 is released as 0.3.3701, which
# every reader running 0.3.37 or an earlier build of it sees as newer, and
# which the next upstream version's first build (0.3.3801) is newer than.
#
# The environment says whose build this is, and the reader it is installed on
# will take platform updates from nowhere else:
#   COBALT_RELEASE_REPOSITORY  owner/repository whose GitHub releases it reads
#   COBALT_PLATFORM_KEY        the public key of SEED_FILE
# and, as for any device build, CC_armv7_unknown_linux_musleabihf names a
# musl C compiler (see .github/workflows/owner-release.yml).
#
# OUT_DIR receives cobalt-VERSION-KoboRoot.tgz, cobalt-device-manifest.txt and
# its .sig: the three assets of a release tagged vVERSION.
set -eu
[ "$#" -eq 3 ] || { echo "usage: owner-release.sh BUILD SEED_FILE OUT_DIR" >&2; exit 2; }
build=$1
seed=$2
out=$3
: "${COBALT_RELEASE_REPOSITORY:?set COBALT_RELEASE_REPOSITORY to owner/repository}"
: "${COBALT_PLATFORM_KEY:?set COBALT_PLATFORM_KEY to the public key of the seed}"
case $build in
    ''|*[!0-9]*) echo "BUILD must be a number from 1 to 99" >&2; exit 2 ;;
esac
[ "$build" -ge 1 ] && [ "$build" -le 99 ] || { echo "BUILD must be 1 to 99" >&2; exit 2; }
cd "$(dirname "$0")/.."

upstream=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$upstream" ] || { echo "Cargo.toml has no X.Y.Z workspace version" >&2; exit 1; }
patch=${upstream##*.}
[ "$patch" -lt 100000 ] || {
    echo "Cargo.toml already holds an owner version ($upstream); restore it first" >&2
    exit 1
}
version="${upstream%.*}.${patch}$(printf '%02d' "$build")"
source=$(git rev-parse HEAD)
git diff --quiet -- Cargo.toml Cargo.lock || {
    echo "Cargo.toml or Cargo.lock has uncommitted changes" >&2
    exit 1
}
# The version is the release's, not the tree's: put the tree back afterwards.
trap 'git checkout -q -- Cargo.toml Cargo.lock' EXIT HUP INT TERM

# The host command is built before the version changes, with the stock
# identity, so it is the same tool whichever release it is signing.
env -u COBALT_RELEASE_REPOSITORY -u COBALT_PLATFORM_KEY cargo build --locked -p kobo-cli
kobo=${CARGO_TARGET_DIR:-target}/debug/kobo
signer=$("$kobo" app-key --seed "$seed" | tail -n 1)
[ "$signer" = "$COBALT_PLATFORM_KEY" ] || {
    echo "the seed's public key is $signer, not COBALT_PLATFORM_KEY" >&2
    exit 1
}

sed -i.bak "s/^version = \"$upstream\"\$/version = \"$version\"/" Cargo.toml
rm -f Cargo.toml.bak
cargo update --workspace --offline

mkdir -p "$out"
package="$out/cobalt-$version-KoboRoot.tgz"
"$kobo" package --out "$package"
"$kobo" inspect "$package" > /dev/null
"$kobo" private-store platform --package "$package" --version "$version" \
    --source "$source" --seed "$seed" --out "$out"
echo "release v$version from $source is in $out"
