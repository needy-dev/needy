#!/usr/bin/env python3
"""Actual Docker/CLI acceptance, without a model account."""
import json
import subprocess
import time
import sys
import uuid
from rust_test_support import Host, archive, extract, wait_for

host = Host(sys.argv[1] if len(sys.argv) > 1 else "target/debug/needy",
            sys.argv[2] if len(sys.argv) > 2 else "needy-rust-test:dev")
children = []
try:
    host.start()
    sid = host.session("workspace")
    host.call("PUT", f"/v1/sessions/{sid}/workspace", archive({"source.txt": b"source\n"}))
    host.call("PUT", f"/v1/sessions/{sid}/workspace", archive({"../outside": b"rejected"}), expected=400)
    key = str(uuid.uuid4())
    command = ["sh", "-c", 'test "$(id -u)" = 1000; cat source.txt; printf "exact result\\n" > result.txt; printf diagnostic >&2']
    run = host.run(sid, command, key=key)
    assert host.run(sid, command, key=key)["id"] == run["id"]
    host.run(sid, ["sh", "-c", "false"], key=key, expected=409)
    result = host.done(run["id"])
    assert result["state"] == "succeeded", result
    data = host.call("GET", f"/v1/sessions/{sid}/workspace", raw=True)
    assert extract(data, "result.txt") == b"exact result\n"
    host.call("PUT", f"/v1/sessions/{sid}/workspace", archive({"late": b"no"}), expected=409)
    print("PASS Docker UID, upload/download, unsafe paths and idempotent run creation", flush=True)

    early = host.run(sid, ["sh", "-c", "exec 1>&-; exec 2>&-; sleep 0.3; exit 0"])
    assert host.done(early["id"])["state"] == "succeeded", "early stdout EOF changed process outcome"
    print("PASS workload closing output before exit", flush=True)

    for name, program in [
        ("unterminated line", "process.stdout.write('x'.repeat((8<<20)+1))"),
        ("cumulative output", "for(let i=0;i<20;i++) console.log('x'.repeat(1<<20))"),
    ]:
        limit = host.run(sid, ["node", "-e", program])
        assert host.done(limit["id"])["state"] == "failed", name
    print("PASS runner bounds oversized unterminated lines and cumulative output", flush=True)

    durable = host.session("durable")
    run = host.run(durable, ["sh", "-c", "echo start >> starts.txt; echo before; sleep 3; echo done > result.txt; echo after"])
    wait_for(lambda: host.call("GET", f"/v1/runs/{run['id']}")["state"] == "running", "run not started")
    event_client = subprocess.Popen([host.binary, "--config", str(host.profile), "events", run["id"], "--follow"],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    children.append(event_client)
    time.sleep(0.2)
    event_client.kill()
    before = event_client.communicate(timeout=5)[0].splitlines()
    cursor = json.loads(before[-1])["id"] if before else 0
    host.daemon.kill()
    host.daemon.wait(timeout=10)
    host.start()
    assert host.done(run["id"])["state"] == "succeeded"
    data = host.call("GET", f"/v1/sessions/{durable}/workspace", raw=True)
    assert extract(data, "starts.txt") == b"start\n", "daemon restarted workload"
    assert extract(data, "result.txt") == b"done\n"
    replay = host.call("GET", f"/v1/runs/{run['id']}/events?after={cursor}", raw=True).splitlines()
    assert replay and all(json.loads(line)["id"] > cursor for line in replay)
    print("PASS batch survives client/daemon SIGKILL without repeating side effects", flush=True)

    a, b, c = [host.session("concurrency") for _ in range(3)]
    first = host.run(a, ["sh", "-c", "sleep 10"])
    second = host.run(b, ["sh", "-c", "sleep 10"])
    host.run(c, ["true"], expected=409)
    host.run(a, ["true"], expected=409)
    host.call("DELETE", f"/v1/sessions/{a}", expected=409)
    for run in (first, second):
        host.call("POST", f"/v1/runs/{run['id']}/stop")
        assert host.done(run["id"])["state"] == "cancelled"
    print("PASS host/session concurrency and confirmed cancellation", flush=True)

except BaseException:
    sys.stderr.write(host.diagnostics())
    raise
finally:
    for process in children:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
    host.close()
