#!/usr/bin/env python3
"""Actual ARM64 Linux exec interception to an actual macOS host process."""
import json
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import termios
import tty

utility_image = os.environ.get("NEEDY_LINUX_IMAGE", "needy-rust-test:dev")
root = Path.cwd().resolve()
mac_binary = root / "target/debug/needy"
linux_binary = root / ".needy/linux-arm64/aarch64-unknown-linux-musl/debug/needy"
fixture = root / ".needy/kernel-fixture-arm64"
assert mac_binary.is_file() and linux_binary.is_file() and fixture.is_file()
with tempfile.TemporaryDirectory(prefix="needy-two-machine-") as temporary:
    directory = Path(temporary).resolve()
    workspace = directory / "workspace"
    workspace.mkdir()
    def openssl(*args):
        subprocess.run(["openssl", *map(str, args)], check=True, capture_output=True)
    ca = directory / "ca.crt"
    ca_key = directory / "ca.key"
    key = directory / "server.key"
    cert = directory / "server.crt"
    csr = directory / "server.csr"
    extension = directory / "extensions.cnf"
    extension.write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:host.docker.internal,DNS:localhost,IP:127.0.0.1\n")
    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Needy test CA", "-keyout", ca_key, "-out", ca)
    openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=host.docker.internal", "-keyout", key, "-out", csr)
    openssl("x509", "-req", "-in", csr, "-CA", ca, "-CAkey", ca_key, "-CAcreateserial", "-days", "1", "-extfile", extension, "-out", cert)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    log = open(directory / "host.log", "w+")
    daemon = subprocess.Popen([str(mac_binary), "daemon", "--no-docker", "--data-dir", str(directory / "host"),
                               "--listen", f"0.0.0.0:{port}", "--tls-cert", str(cert), "--tls-key", str(key),
                               "--host-root", str(workspace), "--allow-host-exec", "uname,sh"],
                              stdout=subprocess.DEVNULL, stderr=log)
    base = ["docker", "run", "--rm", "--init", "--cap-drop=ALL", "--security-opt=seccomp=unconfined", "--security-opt=no-new-privileges",
            "--user", f"{os.getuid()}:{os.getgid()}", "-v", f"{root}:/work:ro", "-v", f"{directory}:/acceptance",
            "-w", "/tmp", "--entrypoint", "/work/" + str(linux_binary.relative_to(root)), utility_image,
            "--config", "/acceptance/client.json"]
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            assert daemon.poll() is None, "Mac daemon exited"
            try:
                connection = socket.create_connection(("127.0.0.1", port), timeout=0.2)
                connection.close()
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise AssertionError("Mac daemon did not listen")
        subprocess.run([str(mac_binary), "pairing-code", "--data-dir", str(directory / "host"), "--out", str(directory / "code")], check=True, capture_output=True)
        pair = subprocess.run([*base, "pair", "--url", f"https://host.docker.internal:{port}", "--ca-cert", "/acceptance/ca.crt", "--code-file", "/acceptance/code"], capture_output=True, timeout=30)
        assert pair.returncode == 0, pair.stderr.decode()
        print("PASS Linux client pairs with real macOS HTTPS daemon", flush=True)

        def intercept(select, command, expected_output, expected_code=0):
            result = subprocess.run([*base, "intercept", "--remote-cwd", str(workspace), "--select", select, "--", *command],
                                    capture_output=True, timeout=30)
            assert result.returncode == expected_code, result.stderr.decode()
            assert result.stdout == expected_output, (result.stdout, result.stderr)
            assert b"1 redirected" in result.stderr, result.stderr
            return result

        platform = subprocess.check_output(["uname", "-s"]).strip()
        intercept("uname", ["/bin/sh", "-c", 'printf "root="; cat /proc/sys/kernel/ostype; printf "remote="; /usr/bin/uname -s'],
                  b"root=Linux\nremote=" + platform + b"\n")
        print("PASS root process stays Linux and selected uname executes on actual macOS", flush=True)
        result = intercept("sh", ["/bin/sh", "-c", "printf 'bridged stdin\\n' | /bin/sh -c 'cat; printf diagnostic >&2; exit 7'; test $? -eq 7"], b"bridged stdin\n")
        assert b"diagnostic" in result.stderr
        print("PASS cross-machine pipeline, stdin, stderr and nonzero exit", flush=True)
        intercept("sh", ["/work/" + str(fixture.relative_to(root)), "signal-status"], b"")
        print("PASS Linux parent observes WIFSIGNALED/SIGTERM from actual Mac child", flush=True)
        intercept("sh", ["/work/" + str(fixture.relative_to(root)), "signal-user"], b"")
        print("PASS SIGUSR1 is mapped by name across different macOS/Linux signal numbers", flush=True)

        master, slave = pty.openpty()
        tty.setraw(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        command = [*base[:2], "-i", "-t", *base[2:], "intercept", "--remote-cwd", str(workspace), "--select", "sh", "--", "/bin/sh", "-c",
                   "/bin/sh -c 'uname -s; stty size; read answer; stty size; trap \"exit 42\" INT; printf ready-for-interrupt; while :; do sleep 1; done'; result=$?; printf 'remote-status=%s\\n' \"$result\"; test \"$result\" -eq 42"]
        client = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        output = b""
        def until(marker):
            global output
            deadline = time.monotonic() + 30
            while marker not in output and time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], 0.2)
                if ready:
                    output += os.read(master, 65536)
            assert marker in output, output
        try:
            until(b"24 80")
            assert platform in output, output
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 37, 111, 0, 0))
            os.kill(client.pid, signal.SIGWINCH)
            time.sleep(0.2)
            os.write(master, b"continue\n")
            until(b"37 111")
            until(b"ready-for-interrupt")
            os.write(master, b"\x03")
            until(b"remote-status=42")
            assert client.wait(timeout=15) == 0, output
            print("PASS interactive Linux proxy to Mac PTY, resize and terminal Ctrl-C", flush=True)
        finally:
            if client.poll() is None:
                client.kill()
                client.wait(timeout=10)
            os.close(master)

        # An outer process keeps the Linux terminal alive while its supervisor
        # is cancelled, so restoration cannot be hidden by container exit.
        marker = workspace / "cancel-ready"
        invocation = ["/work/" + str(linux_binary.relative_to(root)), "--config", "/acceptance/client.json", "intercept", "--remote-cwd", str(workspace), "--select", "sh", "--", "/bin/sh", "-c",
                      f"/bin/sh -c 'touch {marker}; sleep 60'"]
        program = """
const fs=require('fs'), cp=require('child_process');
const settings=()=>cp.execFileSync('stty',['-g'],{stdio:['inherit','pipe','inherit']}).toString();
const before=settings();
const child=cp.spawn(INVOCATION[0],INVOCATION.slice(1),{stdio:'inherit'});
const start=Date.now();
const timer=setInterval(()=>{
  if(fs.existsSync('/acceptance/workspace/cancel-ready')) {clearInterval(timer);child.kill('SIGTERM');}
  else if(Date.now()-start>15000) {clearInterval(timer);child.kill('SIGKILL');process.exitCode=1;}
},50);
child.on('exit',(code)=>{
 clearInterval(timer);
 if(code!==1||settings()!==before) {console.error('terminal restoration failed');process.exitCode=1;}
 else console.log('terminal-restored-after-cancellation');
});
""".replace("INVOCATION", json.dumps(invocation))
        index = base.index("--entrypoint")
        command = [*base[:2], "-i", "-t", *base[2:index], "--entrypoint", "node", utility_image, "-e", program]
        master, slave = pty.openpty()
        tty.setraw(slave)
        client = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        output = b""
        try:
            until(b"terminal-restored-after-cancellation")
            assert client.wait(timeout=15) == 0, output
            print("PASS Linux terminal settings are restored after supervisor cancellation", flush=True)
        finally:
            if client.poll() is None:
                client.kill()
                client.wait(timeout=10)
            os.close(master)
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
