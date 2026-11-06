import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def wait_for(predicate, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(description)


def archive(files):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as tar:
        for name, content in files.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(content)
            entry.mode = 0o644
            tar.addfile(entry, io.BytesIO(content))
    return output.getvalue()


def extract(data, filename):
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        entries = [entry for entry in tar if entry.name.removeprefix("./") == filename]
        assert len(entries) == 1, filename
        return tar.extractfile(entries[0]).read()


class Host:
    def __init__(self, binary, image, extra=()):
        self.binary = str(Path(binary).resolve())
        self.temporary = tempfile.TemporaryDirectory(prefix="needy-rust-docker-")
        self.root = Path(self.temporary.name)
        self.state = self.root / "state"
        self.profile = self.root / "client.json"
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        self.port = listener.getsockname()[1]
        listener.close()
        self.url = f"http://127.0.0.1:{self.port}"
        self.args = [self.binary, "daemon", "--data-dir", str(self.state), "--listen", f"127.0.0.1:{self.port}",
                     "--image", image, "--max-concurrent", "2", *map(str, extra)]
        self.log = open(self.root / "daemon.log", "w+")
        self.daemon = None
        self.token = None
        self.environment = os.environ.copy()

    def start(self):
        self.daemon = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=self.log, env=self.environment)

        def ready():
            assert self.daemon.poll() is None, "daemon exited during startup"
            try:
                urllib.request.urlopen(self.url + "/v1/info", timeout=0.3)
            except urllib.error.HTTPError as error:
                return error.code == 401
            except OSError:
                return False
            return False

        wait_for(ready, "daemon did not listen")
        if self.token is None:
            code = self.root / "code"
            self.cli("pairing-code", "--data-dir", self.state, "--out", code)
            self.cli("pair", "--url", self.url, "--code-file", code)
            self.token = json.loads(self.profile.read_text())["token"]
        return self

    def cli(self, *args, input=b"", expected=0, timeout=40):
        result = subprocess.run([self.binary, "--config", str(self.profile), *map(str, args)], input=input,
                                capture_output=True, timeout=timeout)
        assert result.returncode == expected, (args, result.returncode, result.stderr.decode(errors="replace"))
        return result

    def call(self, method, path, body=None, expected=200, key=None, raw=False):
        data = body if isinstance(body, bytes) else json.dumps(body).encode() if body is not None else None
        headers = {"Authorization": "Bearer " + self.token}
        if data is not None:
            headers["Content-Type"] = "application/x-tar" if isinstance(body, bytes) else "application/json"
        if key is not None:
            headers["Idempotency-Key"] = key
        request = urllib.request.Request(self.url + path, data=data, method=method, headers=headers)
        try:
            response = urllib.request.urlopen(request, timeout=30)
        except urllib.error.HTTPError as error:
            assert error.code == expected, (path, error.code, error.read().decode())
            return None
        assert response.status == expected, (path, response.status)
        data = response.read()
        return data if raw else json.loads(data)

    def session(self, name="fixture"):
        return self.call("POST", "/v1/sessions", {"name": name})["id"]

    def run(self, session, command=None, stdin="", key=None, expected=200):
        body = {"command": command or [], "stdin": stdin}
        return self.call("POST", f"/v1/sessions/{session}/runs", body, expected=expected, key=key or str(uuid.uuid4()))

    def done(self, run):
        result = None

        def finished():
            nonlocal result
            result = self.call("GET", f"/v1/runs/{run}")
            return result["state"] in ("succeeded", "failed", "cancelled", "lost")

        wait_for(finished, "run did not terminate", timeout=60)
        return result

    def container(self, run):
        return json.loads((self.state / "rust-state.json").read_text())["runs"][run]["container"]

    def close(self):
        if self.daemon is not None and self.daemon.poll() is None:
            self.daemon.terminate()
            try:
                self.daemon.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.daemon.kill()
                self.daemon.wait()
        path = self.state / "rust-state.json"
        if path.exists():
            instance = json.loads(path.read_text())["instance_id"]
            owned = "label=io.needy.instance=" + instance
            containers = subprocess.check_output(["docker", "container", "ls", "-aq", "--filter", owned]).decode().split()
            if containers:
                subprocess.run(["docker", "rm", "-f", *containers], check=True, capture_output=True)
            volumes = subprocess.check_output(["docker", "volume", "ls", "-q", "--filter", owned]).decode().split()
            if volumes:
                subprocess.run(["docker", "volume", "rm", *volumes], check=True, capture_output=True)
        self.log.close()
        self.temporary.cleanup()

    def diagnostics(self):
        self.log.flush()
        self.log.seek(0)
        return self.log.read()
