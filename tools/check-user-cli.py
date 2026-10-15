#!/usr/bin/env python3
"""Exercise the real hidden-password CLI using a private pseudo-terminal."""
import argparse
import base64
import os
from pathlib import Path
import pty
import select
import subprocess
import tempfile
import termios
import time


def terminal_run(binary, env, username, cancel=False):
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    process = subprocess.Popen([binary, "users", "create", username],
                               stdin=slave, stdout=slave, stderr=slave, env=env)
    output = bytearray()
    password = b"synthetic TTY password"
    deadline = time.monotonic() + 20

    def until(marker):
        while marker not in output:
            if time.monotonic() >= deadline:
                raise AssertionError("CLI prompt timeout")
            if select.select([master], [], [], 0.1)[0]:
                output.extend(os.read(master, 4096))
            if process.poll() is not None and marker not in output:
                raise AssertionError("CLI exited before its prompt")

    try:
        until(b"Password: ")
        assert not termios.tcgetattr(slave)[3] & termios.ECHO
        if cancel:
            os.write(master, b"\x03")
        else:
            os.write(master, password + b"\n")
            until(b"Confirm password: ")
            os.write(master, password + b"\n")
        status = process.wait(timeout=20)
        while select.select([master], [], [], 0.1)[0]:
            output.extend(os.read(master, 4096))
        assert (status != 0) if cancel else (status == 0)
        assert password not in output, "terminal echoed the password"
        assert termios.tcgetattr(slave) == original, "terminal flags were not restored"
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        termios.tcsetattr(slave, termios.TCSANOW, original)
        os.close(master)
        os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="syncserver-cli-") as work:
        root = Path(work)
        signing = root / "signing.key"
        signing.write_text(base64.b64encode(os.urandom(32)).decode())
        signing.chmod(0o600)
        env = {key: value for key, value in os.environ.items() if not key.startswith("SYNC_")}
        env.update(SYNC_DATA_DIR=str(root / "data"), SYNC_SIGNING_KEY_FILE=str(signing))
        terminal_run(binary, env, "alice")
        terminal_run(binary, env, "cancelled", cancel=True)
        listed = subprocess.run([binary, "users", "list"], env=env,
                                capture_output=True, check=True).stdout
        assert b"alice\t" in listed and b"cancelled\t" not in listed
    print("PASS: real CLI password confirmation/no echo; Ctrl+C cancellation restores terminal")


if __name__ == "__main__":
    main()
