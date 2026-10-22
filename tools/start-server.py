#!/usr/bin/env python3
"""Build and restart this checkout's Linux server, keeping logs in the terminal."""
import argparse
import base64
import fcntl
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def say(message):
    print(f"start: {message}", flush=True)


def instances(binaries):
    """Only signal server processes from this checkout, never users/logs commands."""
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            executable = os.readlink(entry / 'exe').removesuffix(' (deleted)')
            args = (entry / 'cmdline').read_bytes().rstrip(b'\0').split(b'\0')
            if executable in binaries and len(args) == 1:
                # pidfd pins process identity even if the PID is subsequently reused.
                found.append((int(entry.name), os.pidfd_open(int(entry.name))))
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('dev', 'release'), nargs='?', default='dev')
    args = parser.parse_args()
    os.chdir(ROOT)
    os.umask(0o077)
    env = os.environ.copy()
    local = ROOT / 'local-run'
    local.mkdir(mode=0o700, exist_ok=True)
    # Serialize build/stop/start across simultaneous invocations and both profiles.
    with (local / 'start.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        env.setdefault('SYNC_DATA_DIR', str(local / 'data'))
        if 'SYNC_SIGNING_KEY_FILE' not in env:
            key = local / 'signing.key'
            try:
                fd = os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            except FileExistsError:
                pass
            else:
                with os.fdopen(fd, 'w') as stream:
                    stream.write(base64.b64encode(secrets.token_bytes(32)).decode() + '\n')
            env['SYNC_SIGNING_KEY_FILE'] = str(key)
        env.setdefault('SYNC_LOG_LEVEL', 'debug' if args.mode == 'dev' else 'info')
        env.setdefault('SYNC_LOG_PAYLOAD', 'on' if args.mode == 'dev' else 'off')
        metadata = json.loads(subprocess.check_output(
            ['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], env=env))
        target = Path(metadata['target_directory'])
        build = ['cargo', 'build', '--locked', '--bin', 'syncserver']
        if args.mode == 'release':
            build.append('--release')
        say(f'building {args.mode}; existing server stays running until build succeeds')
        subprocess.run(build, env=env, check=True)
        binaries = {str(target / profile / 'syncserver') for profile in ('debug', 'release')}
        old = instances(binaries)
        try:
            for pid, fd in old:
                say(f'stopping PID {pid} with SIGTERM')
                try:
                    signal.pidfd_send_signal(fd, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            import select
            deadline = time.monotonic() + 75
            for pid, fd in old:
                if not select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]:
                    raise RuntimeError(f'PID {pid} did not stop within 75 seconds; restart aborted')
        finally:
            for _, fd in old:
                os.close(fd)
        binary = target / ('release' if args.mode == 'release' else 'debug') / 'syncserver'
        say(f'starting {args.mode}; data={env["SYNC_DATA_DIR"]}; Ctrl+C stops the server')
        # A separate process group lets this supervisor forward terminal signals once.
        child = subprocess.Popen([str(binary)], env=env, start_new_session=True)
        def forward(signum, _frame):
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass
        signal.signal(signal.SIGINT, forward)
        signal.signal(signal.SIGTERM, forward)
        # Keep concurrent starters from missing the process before exec completes.
        # Popen returns only after the executable has been entered successfully.
        fcntl.flock(lock, fcntl.LOCK_UN)
    return child.wait()


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f'start: {error}', file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
