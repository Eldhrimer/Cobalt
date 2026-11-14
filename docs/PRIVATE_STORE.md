# A private catalog beside the Store

A reader can read one more signed catalog beside the public Store's: your own,
from a server you run, for applications you do not publish. They are listed in
Store marked **Private.**, and install, update and uninstall like any other.

This is a change to the runtime, so it needs a Cobalt built from a source tree
that carries it. A stock release ignores the source file.

## What is trusted

- The public Store is unchanged. Its catalog and packages answer to the release
  key alone, and with no source file on the reader nothing here runs.
- Your catalog is fetched, verified and cached separately under your key. It
  can never stand in for the public catalog, and a package it lists must be
  signed with your key, not the release key and not anyone else's.
- An identity the public catalog lists is the public catalog's. Your entry of
  the same name is not offered and cannot be installed. Give your applications
  identities the public Store is unlikely to use.
- An installed application is believed when its manifest is signed by the
  release key or by your key.

Whoever holds your signing seed can sign applications your reader will run.
Keep it on your computer, not on the reader and not on the server.

## Set it up

```sh
# Once: a signing seed. The public key is printed.
kobo private-store keygen --seed ~/.config/cobalt/private-store.seed

# Each release: build, sign and lay out the catalog and packages.
node tools/collect-app-registry.mjs --out target/app-registry.json
kobo private-store publish --app my-app \
    --registry target/app-registry.json \
    --seed ~/.config/cobalt/private-store.seed \
    --out target/private-store \
    --base-url https://apps.example.net/kobo-catalog

# Copy target/private-store/* to the server so each file answers at
# <base-url>/<file name>, then once per reader:
kobo private-store set \
    --url https://apps.example.net/kobo-catalog/cobalt-app-catalog-v2.json \
    --seed ~/.config/cobalt/private-store.seed \
    --device <reader address>
```

The applications are ordinary workspace members under `apps/`. Their source
can live anywhere, such as a private repository linked into `apps/` for the
build, so a public tree never holds them.

`publish` builds each application for the reader, so it needs the ARM
toolchain `kobo build` needs. Every changed application needs a new version in
its `cobalt-app.json`, or readers that have it will not see an update.

Open Store and refresh. `kobo private-store show --device <address>` prints the
source and how its last refresh went (`ok: N application(s)`, or `failed:` and
why: `Integrity` is a wrong key or an altered file, `Unreachable` and
`NotFound` are the server).

## The server

Any static HTTPS host works. The reader asks for the catalog, the same address
with `.sig`, and each package's address from the catalog. Packages are signed,
so the server cannot change what installs.

To keep the packages to yourself, have the server ask for a password. On the
reader, open **Settings ▸ Private store ▸ Type password**. The runtime saves it
where no application can read it, and Store sends it as
`Authorization: Bearer <password>` to the server your catalog's address names
and to no other server. Package addresses must be on that same server to be
sent it. With Caddy, the catalog stays readable and the packages need the
password:

```
apps.example.net {
    handle_path /kobo-catalog/* {
        root * /srv/kobo-catalog
        @locked {
            path *.cobalt-app
            not header Authorization "Bearer {$KOBO_CATALOG_TOKEN}"
        }
        respond @locked 401
        file_server
    }
}
```

Make sure the server cannot start with `KOBO_CATALOG_TOKEN` unset, or the check
compares against an empty password. A wrong or missing password shows on the
reader as an install that fails; the catalog still lists the application.

## Stopping

- `kobo private-store remove --device <address>` stops the reader asking your
  server and offering its applications. What is installed keeps running.
- `kobo private-store remove --forget-key --device <address>` also stops
  trusting the key. Uninstall your applications first: an installed
  application whose signature no longer verifies makes the reader refuse to
  list what is installed until the key is named again. Changing the key with
  `set` has the same effect on applications signed with the old one.

## Limits

- One private source per reader.
- A stock runtime ignores the source file, so the reader must not take stock
  platform updates. Build your Cobalt as your own (next section) and it never
  will.
- Browser install links only know the public catalog.
- A failed refresh of your catalog is recorded for `show` and is not reported
  on the panel; the public catalog's refresh is the one Store reports.

## Your own platform updates

A Cobalt compiled with two variables set is yours: its reader takes platform
updates only from your repository's GitHub releases, signed with your key, in
the background and from Settings alike. It never installs a stock release.
The public Store's catalog still answers to the release key.

```sh
export COBALT_RELEASE_REPOSITORY=you/Cobalt
export COBALT_PLATFORM_KEY=<public key of your seed>
```

Set both or neither; a build with only one does not compile. Without them the
build is a stock one.

Versions are the upstream version with a two-digit build number on the last
part: upstream 0.3.37 is released as 0.3.3701, then 0.3.3702, and the first
build of 0.3.38 is 0.3.3801. Each is newer to the updater than the one before.

`.github/workflows/owner-release.yml` publishes one: run it by hand after
merging an upstream release. It needs the secret `COBALT_PRIVATE_STORE_SEED`
and the variable `COBALT_PLATFORM_KEY`. By hand it is
`tools/owner-release.sh BUILD SEED_FILE OUT_DIR`, then a release tagged
`vVERSION` holding the three files it writes. The reader's update channel must
be Stable; your releases have no beta stream.

The first install of your build is by hand: copy
`cobalt-VERSION-KoboRoot.tgz` from the release to `.kobo/KoboRoot.tgz` on the
reader over USB and eject it. After that it updates itself.

## Checking it

```sh
cargo test -p kobod private
cargo build -p kobo-cli && cargo build -p kobo-sim --example signed-store
scripts/quality/check-private-store-sim.py --output target/private-store-check
```

The simulator check publishes a workspace application under a fresh key and
drives the real Store app: no source, a source under the wrong key, the right
key, install, restart, and removing the source.
