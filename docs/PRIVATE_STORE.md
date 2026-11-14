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
    --base-url https://apps.example.net/<long-random-path>

# Copy target/private-store/* to the server so each file answers at
# <base-url>/<file name>, then once per reader:
kobo private-store set \
    --url https://apps.example.net/<long-random-path>/cobalt-app-catalog-v2.json \
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
with `.sig`, and each package's address from the catalog; it sends no
credentials. Packages are signed, so the server cannot change what installs,
but anyone who knows the address can download them. Put the files under a long
random path and treat that address as the secret. With Caddy:

```
apps.example.net {
    root * /srv/kobo-apps
    file_server
}
```

with the published folder at `/srv/kobo-apps/<long-random-path>/`.

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
- Platform updates come from the public releases. Installing one replaces the
  runtime with a stock one, which ignores the source file: your applications
  then fail verification until a runtime with this change is installed again.
  Turn off automatic platform updates in Settings, and after each upstream
  release rebuild from your tree and install it with `kobo deploy`.
- Browser install links only know the public catalog.
- A failed refresh of your catalog is recorded for `show` and is not reported
  on the panel; the public catalog's refresh is the one Store reports.

## Checking it

```sh
cargo test -p kobod private
cargo build -p kobo-cli && cargo build -p kobo-sim --example signed-store
scripts/quality/check-private-store-sim.py --output target/private-store-check
```

The simulator check publishes a workspace application under a fresh key and
drives the real Store app: no source, a source under the wrong key, the right
key, install, restart, and removing the source.
