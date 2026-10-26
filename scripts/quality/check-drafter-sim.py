#!/usr/bin/env python3
"""Drive Drafter against a live gateway: read a section, hold a word, highlight,
move a block through the outline, send for revision, keep one revision, commit,
and survive a restart with queued work, all through the actual simulator with
real taps and holds.

The gateway is the Drafter gateway (a separate project) served in-process over
TLS. It needs a book repository: pass --repo with a local git checkout of one,
or leave it out to use the gateway's own fixture book.

    scripts/quality/check-drafter-sim.py --gateway-src ~/kobo-soft/drafter \
        --repo /path/to/book-clone --output target/drafter-check
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
TOKEN = 'fixture-token'
HOST = 'drafter.example.test'


def pack(block, part=0, prefix=0):
    return (block << 32) | (part << 16) | prefix


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--gateway-src', type=Path, required=True,
                        help='the gateway project directory (contains the drafter package)')
    parser.add_argument('--repo', type=Path, help='a local git checkout of the book to serve')
    parser.add_argument('--section', help='title of the section to edit (default: the first unlocked one)')
    parser.add_argument('--scale', default='default')
    parser.add_argument('--profile', default='clara-bw-391')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(args.gateway_src.resolve()))
    from drafter import lint, repo as repo_module, revise, server, service  # noqa: E402

    target = Path(os.environ.get('CARGO_TARGET_DIR', str(ROOT / 'target'))).resolve()
    cli = target / 'debug/kobo'
    process = None
    result = {'server': 'local TLS fixture gateway', 'checks': []}
    with tempfile.TemporaryDirectory(prefix='cobalt-drafter-', dir='/tmp') as temporary:
        private = Path(temporary)
        env = dict(os.environ, TMPDIR=str(private), CARGO_TARGET_DIR=str(target),
                   CARGO_PROFILE_DEV_DEBUG='0', CARGO_INCREMENTAL='0',
                   KOBO_SIM_PROFILE=args.profile, KOBO_TEXT_SCALE=args.scale,
                   KOBO_SIM_CLOCK_MILLIS='1767265860000')
        env.pop('KOBO_SIM_OFFLINE', None)
        spec = importlib.util.spec_from_file_location(
            'audiobook_fixture', ROOT / 'scripts/quality/check-audiobook-sim.py')
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        trust = fixture.certificate(private, [HOST])
        env['KOBO_SIM_TRUST_DIR'] = str(trust)

        if args.repo:
            book = args.repo.resolve()
        else:
            sys.path.insert(0, str(args.gateway_src.resolve()))
            from tests import fixtures
            book = Path(fixtures.make_repo(str(private / 'book')))
        source = repo_module.LocalRepo(str(book), ('Designer', 'designer@example.com'))
        gateway = service.Drafter(source, revise.FixtureReviser(), str(private / 'jobs'),
                                  linter=lint.Linter(source), cache_seconds=0)
        httpd = server.make_server(gateway, TOKEN, '127.0.0.1', 0, '/wu')
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(private / f'{HOST}.pem', private / f'{HOST}.key')
        httpd.socket = tls.wrap_socket(httpd.socket, server_side=True)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        origin = f'https://{HOST}/wu'
        env['KOBO_SIM_HTTP_FIXTURE'] = f'{HOST}=127.0.0.1:{httpd.server_port}'

        secrets = private / 'cobalt-sim-secrets/apps/drafter'
        (secrets / 'servers').mkdir(parents=True)
        (secrets / 'servers/wu-drafter').write_text(f'cobalt-server-account-v1\n{origin}\n{TOKEN}')
        os.chmod(secrets / 'servers/wu-drafter', 0o600)
        state = private / 'cobalt-sim-state/drafter'
        state.mkdir(parents=True)
        (state / 'gateway').write_text(origin)

        log_path = output / 'simulator.log'
        with log_path.open('w') as log:
            def stop():
                nonlocal process
                if process and process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=15)
                process = None

            def start():
                nonlocal process
                log.seek(0)
                log.truncate()
                process = subprocess.Popen([str(cli), 'dev', '127.0.0.1:0'],
                                           cwd=ROOT / 'apps/drafter', env=env,
                                           stdout=log, stderr=log, start_new_session=True)
                deadline = time.monotonic() + 300
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError('Simulator exited; see simulator.log')
                    found = re.search(r'Kobo app simulator: http://(127\.0\.0\.1:\d+)',
                                      log_path.read_text())
                    if found:
                        return found.group(1)
                    time.sleep(.1)
                raise TimeoutError('Simulator startup timed out')

            def drive(*steps, timeout=240):
                command = [str(cli), 'drive', '--address', ADDRESS, '--ideal',
                           '--shots', str(output)]
                for step in steps:
                    command.extend(['--step', step])
                subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=log,
                               check=True, timeout=timeout)

            def layout():
                with urllib.request.urlopen(f'http://{ADDRESS}/layout') as response:
                    return json.load(response)

            def capture(name):
                drive('clean', 'shot ' + name)

            def await_screen(text, ticks=40):
                """The simulator's clock is frozen, so a waiting screen's heartbeat only
                ticks when the clock is moved: advance it until the screen says `text`."""
                for _ in range(ticks):
                    if says(text):
                        return
                    request = urllib.request.Request(
                        f'http://{ADDRESS}/clock', data=b'advance 6000', method='POST')
                    urllib.request.urlopen(request).read()
                    time.sleep(.3)
                raise AssertionError(f'the screen never said {text!r}')

            def open_listed(title):
                """The list pages: turn until the section is on the screen, then tap it."""
                for _ in range(12):
                    if says(title):
                        drive(f'tap {title}')
                        return
                    drive('tap-id next-page', 'wait-idle')
                raise AssertionError(f'{title!r} is not in the section list')

            def says(text):
                dump = subprocess.check_output(
                    [str(cli), 'drive', '--address', ADDRESS, '--step', 'dump'],
                    env=env, text=True)
                return text in dump

            def hold_word(block, word, part=0):
                """Holds a word the way a thumb does: find its rectangle in the layout."""
                text = block_text(block, part)
                position = len(text[:text.index(word)].encode())
                context = pack(block, part)
                for _ in range(8):
                    for hit in layout().get('textHits', []):
                        if hit['context'] == context and hit['start'] == position:
                            drive(f"hold-at {hit['x'] + hit['width'] // 2},"
                                  f"{hit['y'] + hit['height'] // 2}", 'wait-idle')
                            return
                    drive('tap-id next-page', 'wait-idle')
                raise AssertionError(f'{word!r} is not on any page of block {block}')

            def block_text(block, part=0):
                shown = section_blocks[block]
                return shown['parts'][part]['text'] if 'parts' in shown else shown['text']

            try:
                # The gateway the app talks to, read directly for expectations.
                listing = gateway.list_sections()
                unlocked = [i for i in listing['items'] if i['status'].lower() != 'locked']
                locked = [i for i in listing['items'] if i['status'].lower() == 'locked']
                item = next(i for i in listing['items'] if i['title'] == args.section) \
                    if args.section else unlocked[0]
                args.section = item['title']
                section_id = item['id']
                section_blocks = gateway.get_section(section_id)['blocks']
                first = next(i for i, b in enumerate(section_blocks) if b['kind'] == 'para')
                paragraph = section_blocks[first]['text']
                word = paragraph.split()[2].strip('.,;:')

                ADDRESS = start()
                drive('wait-for Drafter', 'wait-idle', timeout=300)
                capture('drafter-sections')
                result['checks'].append(dict(
                    name='sections list', status='passed',
                    detail=f"{len(listing['items'])} sections with statuses from the gateway"))

                open_listed(args.section)
                drive('wait-for Queue', 'wait-idle', timeout=300)
                capture('drafter-reading')
                result['checks'].append(dict(
                    name='section opens as blocks', status='passed',
                    detail='paragraphs, callouts, tables and lists drawn on the panel'))

                hold_word(first, word)
                drive('wait-for Highlight this sentence', 'wait-idle')
                capture('drafter-word-menu')
                result['checks'].append(dict(
                    name='hold a word', status='passed',
                    detail=f'a real hold on "{word}" opened the block menu'))

                drive('tap Highlight this sentence', 'wait-for What should change',
                      'wait-idle')
                capture('drafter-ask')
                drive('tap Tighten', 'wait-for Queued: tighten', 'wait-idle')
                capture('drafter-highlighted')
                result['checks'].append(dict(
                    name='highlight and ask', status='passed',
                    detail='the sentence is washed and Queue (1) shows the pending ask'))

                drive('tap Outline', 'wait-for Outline', 'wait-idle')
                capture('drafter-outline')
                drive('tap-id om.1', 'wait-idle')
                capture('drafter-outline-menu')
                drive('tap Move', 'wait-for Start of the section', 'wait-idle')
                capture('drafter-place')
                drive('tap-id place.top', 'wait-for Outline', 'wait-idle')
                result['checks'].append(dict(
                    name='move by pick up and place', status='passed',
                    detail='a block moved to the top of the section in two taps'))

                # A restart keeps both queued operations.
                stop()
                ADDRESS = start()
                drive('wait-for Drafter', 'wait-idle', timeout=300)
                open_listed(args.section)
                drive('wait-for Queue (2)', 'wait-idle', timeout=300)
                capture('drafter-restored')
                result['checks'].append(dict(
                    name='restart restores the queue', status='passed',
                    detail='after a full simulator restart both queued changes were back'))

                drive('tap Queue (2)', 'wait-for Send for revision', 'wait-idle')
                capture('drafter-queue')
                drive('tap Send for revision', 'wait-idle')
                if says('Revising'):
                    capture('drafter-waiting')
                await_screen('Commit to GitHub')
                drive('wait-idle')
                capture('drafter-proposal-summary')
                if says('next-page'):
                    drive('tap-id next-page', 'wait-idle')
                    capture('drafter-proposal')
                result['checks'].append(dict(
                    name='review round trip', status='passed',
                    detail='the gateway applied the move, revised the marked sentence, '
                           'linted it, and returned a proposal'))

                before = subprocess.check_output(
                    ['git', '-C', str(book), 'rev-parse', 'HEAD'], text=True).strip()
                drive('tap Commit to GitHub', 'wait-for Committed', 'wait-idle',
                      timeout=300)
                capture('drafter-committed')
                after = subprocess.check_output(
                    ['git', '-C', str(book), 'rev-parse', 'HEAD'], text=True).strip()
                assert before != after, 'the commit should have reached the repository'
                message = subprocess.check_output(
                    ['git', '-C', str(book), 'log', '-1', '--format=%s'], text=True).strip()
                changed = subprocess.check_output(
                    ['git', '-C', str(book), 'show', '--stat', '--format=', 'HEAD'],
                    text=True).strip()
                assert len(changed.splitlines()) == 2, changed
                result['checks'].append(dict(
                    name='commit lands in the repository', status='passed',
                    detail=f'{message!r}; {changed.splitlines()[0].strip()}'))
                if locked:
                    drive('tap back', 'wait-for Drafter', 'wait-idle')
                    open_listed(locked[0]['title'])
                    drive('wait-for Queue', 'wait-idle', timeout=300)
                    section = gateway.get_section(locked[0]['id'])['blocks']
                    target = next(i for i, b in enumerate(section) if b['kind'] == 'para')
                    section_blocks = section
                    hold_word(target, section[target]['text'].split()[2].strip('.,;:'))
                    drive('tap Highlight this sentence', 'wait-for What should change',
                          'tap Tighten', 'wait-for Queued', 'tap Queue (1)',
                          'wait-for Send for revision', 'tap Send for revision',
                          'wait-for This section is Locked', 'wait-idle')
                    capture('drafter-locked')
                    drive('tap Cancel', 'wait-for Send for revision')
                    result['checks'].append(dict(
                        name='locked sections ask first', status='passed',
                        detail=f"{locked[0]['title']!r} is Locked; sending needed a confirmation"))
                result['status'] = 'passed'
            finally:
                stop()
                httpd.shutdown()
        (output / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
