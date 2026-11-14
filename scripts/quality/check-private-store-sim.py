#!/usr/bin/env python3
"""Drive the real Store app with an owner's private source beside the Store's.

Publishes one workspace application with `kobo private-store publish` under a
key made for this run, serves it through the simulator's local signed-Store
transport, names it with `kobo private-store set`, and checks on the panel
that a source under the wrong key offers nothing, that the right one is listed
as Private beside the fixture's own catalog, and that both install.

Publishing builds the application for the reader, so this needs the ARM
toolchain `kobo build` needs.

    cargo build -p kobo-cli && cargo build -p kobo-sim --example signed-store
    scripts/quality/check-private-store-sim.py --output target/private-store-check
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
BASE = 'https://apps.example.invalid/own'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--app', default='post', help='the workspace application to publish')
    parser.add_argument('--title', default='Post', help="that application's display name")
    parser.add_argument('--scale', default='default')
    args = parser.parse_args()
    target = Path(os.environ.get('CARGO_TARGET_DIR', str(ROOT / 'target'))).resolve()
    cli = target / 'debug/kobo'
    builder = target / 'debug/examples/signed-store'
    if not cli.is_file() or not builder.is_file():
        parser.error('Build kobo-cli and the kobo-sim signed-store example first.')
    args.output.mkdir(parents=True, exist_ok=True, mode=0o700)
    process = None
    checks = []
    with tempfile.TemporaryDirectory(prefix='cobalt-private-store-', dir='/tmp') as private:
        private = Path(private)
        fixture = private / 'fixture'
        reader = fixture / 'installed'
        env = dict(os.environ, TMPDIR=str(private), CARGO_TARGET_DIR=str(target),
                   CARGO_PROFILE_DEV_DEBUG='0', CARGO_INCREMENTAL='0',
                   KOBO_SIM_PROFILE='clara-bw-391', KOBO_TEXT_SCALE=args.scale,
                   KOBO_SIM_APP_STORE=str(fixture), KOBO_SIM_FIXTURE='original-signed-store',
                   KOBO_SIM_SEED='0', KOBO_SIM_CLOCK_MILLIS='1788850860000')
        with (args.output / 'simulator.log').open('w') as log:
            def run(*command):
                subprocess.run([str(c) for c in command], check=True, cwd=ROOT, env=env,
                               stdout=log, stderr=log, timeout=1800)

            def kobo(*arguments):
                return subprocess.run([str(cli), *[str(a) for a in arguments]], cwd=ROOT, env=env,
                                      capture_output=True, text=True, timeout=60)

            def stop():
                if process is not None and process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)

            try:
                run(builder, 'init', fixture, '1.0.0')

                # The owner's side: a key, and a catalog signed with it.
                seed, stranger = private / 'seed', private / 'stranger-seed'
                run(cli, 'private-store', 'keygen', '--seed', seed)
                run(cli, 'private-store', 'keygen', '--seed', stranger)
                registry, site = private / 'registry.json', private / 'site'
                run('node', 'tools/collect-app-registry.mjs', '--out', registry)
                run(cli, 'private-store', 'publish', '--app', args.app, '--registry', registry,
                    '--seed', seed, '--out', site, '--base-url', BASE)
                # The simulator's transport is a folder of files named by the
                # hash of the address they answer; nothing reaches a network.
                for path in site.iterdir():
                    url = f'{BASE}/{path.name}'
                    blob = fixture / 'transport' / (hashlib.sha256(url.encode()).hexdigest() + '.blob')
                    blob.write_bytes(path.read_bytes())
                catalog = f'{BASE}/cobalt-app-catalog-v2.json'

                def start():
                    nonlocal process
                    start_offset = log.tell()
                    process = subprocess.Popen([str(cli), 'dev', '127.0.0.1:0'],
                                               cwd=ROOT / 'examples/store', env=env,
                                               stdout=log, stderr=log, start_new_session=True)
                    deadline = time.monotonic() + 300
                    while time.monotonic() < deadline:
                        log.flush()
                        text = (args.output / 'simulator.log').read_text()[start_offset:]
                        if process.poll() is not None:
                            raise RuntimeError('Store simulator stopped: ' + text[-2000:])
                        match = re.search(r'Kobo app simulator: http://(127\.0\.0\.1:\d+)', text)
                        if match:
                            return match.group(1)
                        time.sleep(.1)
                    raise RuntimeError('Store simulator did not start')

                address = start()

                def drive(*steps):
                    command = [str(cli), 'drive', '--address', address, '--ideal',
                               '--shots', str(args.output)]
                    for step in steps:
                        command.extend(['--step', step])
                    subprocess.run(command, cwd=ROOT, env=env, check=True, timeout=120,
                                   stdout=log, stderr=log)

                def manifest(app):
                    return json.loads((reader / 'apps' / app / 'manifest.json').read_text())

                def shown():
                    return kobo('private-store', 'show', '--root', reader).stdout

                # With no source named, the Store is the fixture's catalog alone.
                drive('wait-idle', 'expect-state /simulation#/appStore/mode "signed-local"',
                      'tap-id refresh', 'wait-idle', 'expect Quality fixture',
                      f'expect-missing {args.title}', 'expect-missing Private.')
                assert 'No private source is named' in shown(), shown()
                checks.append('no source named: only the Store catalog is listed')

                # A source under a key that did not sign the catalog offers nothing.
                run(cli, 'private-store', 'set', '--url', catalog, '--seed', stranger, '--root', reader)
                drive('tap-id refresh', 'wait-idle', 'expect Quality fixture',
                      f'expect-missing {args.title}', 'expect-missing Private.')
                assert 'failed: Integrity' in shown(), shown()
                assert not (reader / 'store' / 'catalog-private').exists()
                checks.append('wrong key: the catalog is refused and nothing is offered')

                # The owner's key lists it, marked, beside the Store's own entry.
                run(cli, 'private-store', 'set', '--url', catalog, '--seed', seed, '--root', reader)
                drive('tap-id refresh', 'wait-idle', 'expect Quality fixture',
                      f'expect {args.title}', 'expect Private.', 'shot private-listed')
                assert 'ok: 1 application(s)' in shown(), shown()
                checks.append('right key: listed as Private beside the Store catalog')

                drive(f'tap-id app-{args.app}', 'wait-for Permissions', 'expect Not installed',
                      'expect Private.', 'shot private-page',
                      f'tap-id install-{args.app}', 'wait-for is installed.', 'expect Uninstall',
                      'shot private-installed')
                installed = manifest(args.app)
                published = json.loads((site / 'cobalt-app-catalog-v2.json').read_text())
                assert installed == published['entries'][0]['manifest'], installed
                binary = (reader / 'apps' / args.app / 'bin' / f'kobo-{args.app}').read_bytes()
                assert hashlib.sha256(binary).hexdigest() == installed['binary_sha256']
                checks.append('install: the signed manifest and the exact published binary landed')

                # The Store's own catalog still installs, under its own key.
                back = 'tap-id 4294967295'
                drive(back, 'tap-id app-quality-fixture', 'wait-for Permissions',
                      'tap-id install-quality-fixture', 'wait-for is installed.')
                assert manifest('quality-fixture')['version'] == '1.0.0'
                checks.append('the Store catalog installs as before')

                # A restart reads both back from the reader: installed and listed.
                stop()
                address = start()
                drive('wait-idle', 'tap-id filter-all', f'expect {args.title}', 'expect Quality fixture',
                      f'tap-id app-{args.app}', 'expect-missing Not installed', 'expect Uninstall',
                      'shot private-reopened')
                checks.append('restart: both are still installed and verified')

                # Dropping the address keeps what was installed and offers nothing new.
                run(cli, 'private-store', 'remove', '--root', reader)
                drive(back, 'tap-id refresh', 'wait-idle', f'expect {args.title}',
                      'expect-missing Private.', f'tap-id app-{args.app}', 'expect Uninstall',
                      f'tap-id remove-{args.app}', 'wait-for Its saved data stays on this reader',
                      'tap-id confirm-remove', 'wait-for is uninstalled.')
                assert not (reader / 'apps' / args.app).exists()
                drive(back, 'tap-id refresh', 'wait-idle', f'expect-missing {args.title}')
                checks.append('source removed: the installed app kept running, uninstalled cleanly, '
                              'and is no longer offered')

                result = dict(status='passed', app=args.app, scale=args.scale, checks=checks,
                              source_head=subprocess.check_output(
                                  ['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                              source_dirty=bool(subprocess.check_output(
                                  ['git', 'status', '--porcelain'], cwd=ROOT)))
                (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
                print(json.dumps(result, indent=2))
            finally:
                stop()


if __name__ == '__main__':
    main()
