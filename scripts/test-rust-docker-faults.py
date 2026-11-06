#!/usr/bin/env python3
"""Inject faults at the external Docker CLI, while running actual containers."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import io
from rust_test_support import Host, extract, wait_for

case = sys.argv[2] if len(sys.argv) > 2 else "observation"
host = Host(sys.argv[1] if len(sys.argv) > 1 else "target/debug/needy", "needy-rust-test:dev")
directory = host.root / "bin"
directory.mkdir()
real = shutil.which("docker")
trigger = host.root / "trigger"
ack = host.root / "ack"
wrapper = directory / "docker"
operation = {"observation": "logs", "cancel": "create"}.get(case, "never")
wrapper.write_text(f'''#!/usr/bin/env python3
import os, sys, time
from pathlib import Path
trigger=Path({str(trigger)!r})
if trigger.exists() and sys.argv[1]=={operation!r}:
    trigger.unlink()
    Path({str(ack)!r}).write_text("injected")
    if sys.argv[1] in ("logs", "stop"): sys.exit(42)
    time.sleep(2)
os.execv({real!r},[{real!r},*sys.argv[1:]])
''')
wrapper.chmod(0o755)
host.environment["PATH"] = str(directory) + os.pathsep + host.environment["PATH"]
try:
    host.start()
    sid = host.session(case)
    if case == "observation":
        trigger.touch()
        run = host.run(sid, ["sh", "-c", "echo once >> starts.txt; echo before; sleep 3; echo after"])
        wait_for(ack.exists, "log fault was not injected")
        wait_for(lambda: host.call("GET", f"/v1/runs/{run['id']}").get("error"), "observation failure not reported")
        current = host.call("GET", f"/v1/runs/{run['id']}")
        assert current["state"] not in ("lost", "failed", "succeeded"), "running container lost its reservation"
        assert host.call("GET", f"/v1/sessions/{sid}")["active_run_id"] == run["id"]
        assert host.done(run["id"])["state"] == "succeeded"
        data = host.call("GET", f"/v1/sessions/{sid}/workspace", raw=True)
        assert extract(data, "starts.txt") == b"once\n"
        events = host.call("GET", f"/v1/runs/{run['id']}/events", raw=True)
        lines = [json.loads(line) for line in events.splitlines()]
        assert sum(event.get("data", {}).get("line") == "before" for event in lines) == 1
        print("PASS log observation failure retains running reservation and resumes without replay")
    elif case == "cancel":
        trigger.touch()
        run = host.run(sid, ["sh", "-c", "echo must-not-run > forbidden.txt"])
        wait_for(ack.exists, "create delay was not injected")
        host.call("POST", f"/v1/runs/{run['id']}/stop")
        assert host.done(run["id"])["state"] == "cancelled"
        data = host.call("GET", f"/v1/sessions/{sid}/workspace", raw=True)
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            assert all(entry.name.removeprefix("./") != "forbidden.txt" for entry in tar), "cancelled pending command still executed"
        print("PASS cancellation before container creation prevents execution")
    elif case in ("volume", "foreign-volume", "volume-delete"):
        instance = json.loads((host.state / "rust-state.json").read_text())["instance_id"]
        volume = "needy-rs-" + hashlib.sha256(instance.encode()).hexdigest()[:12] + "-" + sid + "-workspace"
        subprocess.run([real, "volume", "rm", volume], check=True, capture_output=True)
        if case == "volume-delete":
            host.call("DELETE", f"/v1/sessions/{sid}")
            assert sid not in [session["id"] for session in host.call("GET", "/v1/sessions")]
            print("PASS session deletion recovers after one volume has already been removed")
            sys.exit(0)
        if case == "foreign-volume":
            # This test alone creates and owns this foreign-labelled resource.
            subprocess.run([real, "volume", "create", "--label", "test.needy.owner=rust-fault-test", volume], check=True, capture_output=True)
        try:
            host.call("GET", f"/v1/sessions/{sid}/workspace", expected=500, raw=True)
            result = subprocess.check_output([real, "volume", "ls", "-q", "--filter", "name=^" + volume + "$"])
            if case == "volume":
                assert not result.strip(), "missing workspace was silently recreated"
            else:
                host.call("DELETE", f"/v1/sessions/{sid}", expected=500)
                labels = json.loads(subprocess.check_output([real, "volume", "inspect", volume]))[0]["Labels"]
                assert labels == {"test.needy.owner": "rust-fault-test"}, "foreign volume was claimed or modified"
                subprocess.run([real, "volume", "inspect", volume.removesuffix("workspace") + "home"], check=True, capture_output=True)
            print("PASS missing/foreign workspace volume is rejected without claiming or replacing data")
        finally:
            if case == "foreign-volume":
                subprocess.run([real, "volume", "rm", volume], check=True, capture_output=True)
    else:
        raise AssertionError("unknown fault case")
except BaseException:
    sys.stderr.write(host.diagnostics())
    raise
finally:
    host.close()
