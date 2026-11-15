#!/bin/sh
# Install this owner's Cobalt on a Kobo plugged in over USB.
#
#   curl -fsSL https://raw.githubusercontent.com/Eldhrimer/Cobalt/main/tools/owner-install.sh | sh
#   sh owner-install.sh [/path/to/the/mounted/Kobo]
#
# It downloads the newest platform release from the repository below, checks
# its signed manifest against the owner's public key and the package against
# the manifest, and then prepares the reader:
#   .kobo/KoboRoot.tgz                    the package; the Kobo installs it
#                                         when it is ejected and restarts
#   .adds/nm/cobalt                       the NickelMenu entry that starts it
#   .adds/cobalt/state/private-store      the owner's private app catalog
# Nothing is written outside the reader, and nothing on the reader is
# replaced without being named above. Re-running it is safe.
set -eu

REPOSITORY="Eldhrimer/Cobalt"
PUBLIC_KEY="8722fe5c387275a4686c8315b702421d174a189f80a915d5bcff3fe8d2557c71"
CATALOG_URL="https://kobo.eldhrimer.cc/apps/store/cobalt-app-catalog-v2.json"
MENU_ENTRY="menu_item :main    :Cobalt    :cmd_spawn    :quiet:/mnt/onboard/.adds/cobalt-launch.sh"

say() { printf '%s\n' "$*"; }
fail() { printf 'owner-install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "this needs the '$1' command"; }

need curl
need perl
OPENSSL=${OPENSSL:-openssl}
need "$OPENSSL"
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    fail "this needs sha256sum or shasum"
fi

# --- the reader -----------------------------------------------------------
reader=${1:-}
if [ -z "$reader" ]; then
    for candidate in /Volumes/KOBOeReader "/media/${USER:-}/KOBOeReader" \
        "/run/media/${USER:-}/KOBOeReader" /mnt/KOBOeReader; do
        if [ -d "$candidate/.kobo" ]; then
            reader=$candidate
            break
        fi
    done
fi
[ -n "$reader" ] || fail "no Kobo found. Plug it in, tap Connect, and pass its folder: sh owner-install.sh /path/to/KOBOeReader"
[ -d "$reader/.kobo" ] || fail "$reader is not a mounted Kobo (it has no .kobo folder)"
say "Reader: $reader"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM

# --- the newest release ---------------------------------------------------
say "Asking $REPOSITORY for its newest release..."
curl -fsSL --max-time 60 "https://api.github.com/repos/$REPOSITORY/releases/latest" > "$work/release.json" \
    || fail "could not read the releases of $REPOSITORY"
asset() {
    # The download address of the release asset whose name matches $1.
    grep -o '"browser_download_url": *"[^"]*"' "$work/release.json" \
        | sed 's/.*"\(https:[^"]*\)"/\1/' | grep -E "/$1\$" | head -n 1
}
manifest_url=$(asset 'cobalt-device-manifest\.txt')
signature_url=$(asset 'cobalt-device-manifest\.txt\.sig')
[ -n "$manifest_url" ] && [ -n "$signature_url" ] \
    || fail "the newest release has no signed device manifest"
curl -fsSL --max-time 60 -o "$work/manifest" "$manifest_url"
curl -fsSL --max-time 60 -o "$work/manifest.sig" "$signature_url"

# --- the signature --------------------------------------------------------
# An Ed25519 public key as OpenSSL reads it: a fixed twelve-byte header, then
# the thirty-two bytes of the key.
perl -e 'print pack("H*", "302a300506032b6570032100" . $ARGV[0])' "$PUBLIC_KEY" > "$work/key.der"
perl -e 'local $/; $_ = <STDIN>; s/\s+//g; /^[0-9a-f]{128}$/ or exit 1; print pack("H*", $_)' \
    < "$work/manifest.sig" > "$work/signature" || fail "the release's signature is malformed"
if ! "$OPENSSL" pkey -pubin -inform DER -in "$work/key.der" -noout 2>/dev/null; then
    fail "this $OPENSSL cannot check Ed25519 signatures. Install OpenSSL 3 and run again with OPENSSL=/path/to/openssl"
fi
"$OPENSSL" pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin \
    -in "$work/manifest" -sigfile "$work/signature" >/dev/null 2>&1 \
    || fail "the release manifest is NOT signed by this owner's key. Nothing was installed."

# --- the manifest ---------------------------------------------------------
[ "$(sed -n 1p "$work/manifest")" = "cobalt-device-release 1" ] || fail "unexpected manifest format"
version=$(sed -n 's/^version \([0-9][0-9.]*\)$/\1/p' "$work/manifest")
device=$(sed -n 's/^device //p' "$work/manifest")
package=$(printf '%s' "$device" | awk '{print $1}')
bytes=$(printf '%s' "$device" | awk '{print $2}')
digest=$(printf '%s' "$device" | awk '{print $3}')
[ -n "$version" ] && [ "$package" = "cobalt-$version-KoboRoot.tgz" ] && [ -n "$digest" ] \
    || fail "the manifest does not name a device package"
package_url=$(asset "$(printf '%s' "$package" | sed 's/\./\\./g')")
[ -n "$package_url" ] || fail "the release has no $package"

say "Downloading Cobalt $version..."
curl -fL --progress-bar --max-time 900 -o "$work/package" "$package_url"
[ "$(wc -c < "$work/package" | tr -d ' ')" = "$bytes" ] || fail "the package is not the size its manifest says"
[ "$(sha256 "$work/package")" = "$digest" ] || fail "the package does not match its signed manifest. Nothing was installed."
say "Verified: signed by the owner's key, package matches the manifest."

# --- the reader's files ---------------------------------------------------
cp "$work/package" "$reader/.kobo/KoboRoot.tgz.part"
mv "$reader/.kobo/KoboRoot.tgz.part" "$reader/.kobo/KoboRoot.tgz"
say "Placed .kobo/KoboRoot.tgz"

if [ -d "$reader/.adds/nm" ]; then
    if grep -qsF "/mnt/onboard/.adds/cobalt-launch.sh" "$reader"/.adds/nm/* 2>/dev/null; then
        say "NickelMenu already has a Cobalt entry"
    else
        printf '%s\n' "$MENU_ENTRY" > "$reader/.adds/nm/cobalt"
        say "Added the Cobalt entry to NickelMenu (.adds/nm/cobalt)"
    fi
else
    say "NickelMenu is not installed on this reader (.adds/nm is missing)."
    say "Install NickelMenu, then run this again to add the Cobalt entry."
fi

source_file="$reader/.adds/cobalt/state/private-store"
wanted=$(printf 'url=%s\nkey=%s\n' "$CATALOG_URL" "$PUBLIC_KEY")
if [ -f "$source_file" ] && [ "$(cat "$source_file")" = "$wanted" ]; then
    say "The private catalog is already named"
else
    if [ -f "$source_file" ]; then
        cp "$source_file" "$source_file.before-install"
        say "Kept the previous private-store file as private-store.before-install"
    fi
    mkdir -p "$reader/.adds/cobalt/state"
    printf '%s\n' "$wanted" > "$source_file"
    say "Named the private catalog (.adds/cobalt/state/private-store)"
fi

sync 2>/dev/null || true
say ""
say "Done. Now:"
say "  1. Eject the Kobo safely and unplug it. It installs Cobalt $version and restarts."
say "  2. Open Cobalt from the Kobo's menu."
say "  3. Settings > Private store > Type password: enter your catalog password."
say "  4. Store > refresh: your apps are listed as Private."
