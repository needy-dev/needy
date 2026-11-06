#!/usr/bin/env python3
"""Actual Rust CLI/HTTP/macOS-or-Linux-process acceptance; no model account."""
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/needy").resolve())


def wait_for(predicate, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.03)
    raise AssertionError(description)


def gone(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True


with tempfile.TemporaryDirectory(prefix="needy-rust-host-") as temporary:
    root = Path(temporary)
    state = root / "state"
    workspace = root / "workspace"
    workspace.mkdir()
    profile = root / "client.json"
    code = root / "code"
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    url = f"http://127.0.0.1:{port}"
    log = open(root / "daemon.log", "w+")

    def start():
        return subprocess.Popen(
            [binary, "daemon", "--data-dir", str(state), "--listen", f"127.0.0.1:{port}",
             "--no-docker", "--host-root", str(workspace), "--allow-host-exec", "uname,sh,cat,stty"],
            stdout=subprocess.DEVNULL, stderr=log,
        )

    def cli(*args, input=b"", expected=0):
        result = subprocess.run([binary, "--config", str(profile), *map(str, args)],
                                input=input, capture_output=True, timeout=20)
        assert result.returncode == expected, (args, result.returncode, result.stderr.decode())
        return result

    daemon = start()
    try:
        wait_for(lambda: (state / "rust-state.json").exists(), "daemon did not prepare state")
        cli("pairing-code", "--data-dir", state, "--out", code)
        original_pairing = (state / "pairing.json").read_bytes()
        cli("pair", "--url", url, "--code-file", code)
        token = json.loads(profile.read_text())["token"]
        info = json.loads(cli("info").stdout)
        assert info["host_execution"] is True
        assert "credential_configured" not in info
        print("PASS pairing and host capability")

        expected_platform = subprocess.check_output(["uname", "-s"]).strip()
        assert cli("exec", "--cwd", workspace, "--", "uname", "-s").stdout.strip() == expected_platform
        result = cli("exec", "--cwd", workspace, "--", "sh", "-c",
                     'cat; printf "diagnostic\\n" >&2; exit 7', input=b"exact stdin\n", expected=7)
        assert result.stdout == b"exact stdin\n", result.stdout
        assert result.stderr == b"diagnostic\n", result.stderr
        print("PASS actual process, stdin, separated stderr and exit status")

        def connect(command, pty=None):
            sock = socket.create_connection(("127.0.0.1", port), timeout=10)
            body = json.dumps({"command": command, "cwd": str(workspace), "environment": {}, "pty": pty}).encode()
            sock.sendall((f"POST /v1/exec HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n"
                          f"Connection: Upgrade\r\nUpgrade: needy-exec-v1\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n\r\n").encode() + body)
            reader = sock.makefile("rb", buffering=0)
            status = reader.readline()
            assert b"101" in status, status
            while reader.readline() != b"\r\n":
                pass
            return sock, reader

        def send(sock, kind, data=b""):
            sock.sendall(struct.pack(">BI", kind, len(data)) + data)

        def exact(reader, count):
            data = b""
            while len(data) < count:
                chunk = reader.read(count - len(data))
                assert chunk, "unexpected tunnel EOF"
                data += chunk
            return data

        def frame(reader):
            kind, length = struct.unpack(">BI", exact(reader, 5))
            assert length <= 8 << 20
            return kind, exact(reader, length)

        sock, reader = connect(["sh", "-c", "stty size; read answer; stty size"], {"rows": 24, "cols": 80})
        output = b""
        while b"24 80" not in output:
            kind, data = frame(reader)
            assert kind == 3, (kind, data)
            output += data
        send(sock, 8, json.dumps({"rows": 37, "cols": 111}).encode())
        send(sock, 1, b"continue\n")
        while True:
            kind, data = frame(reader)
            if kind == 5:
                assert json.loads(data)["code"] == 0
                break
            assert kind == 3, (kind, data)
            output += data
        assert b"37 111" in output, output
        reader.close()
        sock.close()
        print("PASS real PTY and window resize")

        sock, reader = connect(["sh", "-c", "trap 'exit 42' INT; printf ready; while :; do sleep 1; done"], {"rows": 24, "cols": 80})
        assert frame(reader) == (3, b"ready")
        send(sock, 7, b"2")
        while True:
            kind, data = frame(reader)
            if kind == 5:
                assert json.loads(data)["code"] == 42, data
                break
            assert kind in (3, 4), (kind, data)
        reader.close()
        sock.close()
        print("PASS process-group SIGINT")

        def long_running(prefix):
            parent = workspace / f"{prefix}-parent"
            child = workspace / f"{prefix}-child"
            process = subprocess.Popen([binary, "--config", str(profile), "exec", "--cwd", str(workspace), "--", "sh", "-c",
                                        f"echo $$ > {parent.name}; sleep 60 & echo $! > {child.name}; wait"],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_for(lambda: parent.exists() and child.exists(), "host children did not start")
            return process, [int(parent.read_text()), int(child.read_text())]

        process, pids = long_running("disconnect")
        current = json.loads(cli("info").stdout)
        assert current["active_native_runs"] == 1 and current["active_runs"] == 1, current
        process.kill()
        process.communicate(timeout=10)
        wait_for(lambda: all(gone(pid) for pid in pids), "disconnection left host processes alive")
        print("PASS client SIGKILL cleans host process group")

        process, pids = long_running("daemon-kill")
        daemon.kill()
        daemon.wait(timeout=10)
        process.communicate(timeout=10)
        wait_for(lambda: all(gone(pid) for pid in pids), "daemon SIGKILL left host processes alive")
        daemon = start()
        wait_for(lambda: subprocess.run([binary, "--config", str(profile), "info"], capture_output=True).returncode == 0,
                 "daemon did not preserve paired authorization")
        print("PASS daemon SIGKILL cleanup and persisted pairing")

        clients = json.loads(cli("clients", "list").stdout)
        process, pids = long_running("revocation")
        cli("clients", "revoke", clients[0]["id"])
        process.communicate(timeout=10)
        wait_for(lambda: all(gone(pid) for pid in pids), "revocation left host processes alive")
        cli("info", expected=1)
        cli("pair", "--url", url, "--code-file", code, expected=1)
        print("PASS revocation and consumed pairing code")
        second_code = root / "second-code"
        cli("pairing-code", "--data-dir", state, "--out", second_code)
        cli("pair", "--url", url, "--code-file", second_code)
        (state / "pairing.json").write_bytes(original_pairing)
        cli("pair", "--url", url, "--code-file", code, expected=1)
        print("PASS an older consumed code cannot be reused after a second pairing")
    except BaseException:
        log.flush()
        log.seek(0)
        sys.stderr.write(log.read())
        raise
    finally:
        if daemon.poll() is None:
            daemon.terminate()
            daemon.wait(timeout=15)
        log.close()
