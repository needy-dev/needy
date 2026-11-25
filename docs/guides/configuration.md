# Configuration Reference

A profile is YAML; unknown keys are errors, and `extends:` builds on another
profile (including `builtin:opencode`, `builtin:codex`, `builtin:pi`, and
`builtin:herdr`). See [`examples/`](../../examples) for complete profiles, and
run `needy config profile check PROFILE --mode control --cmd "bash -c pwd"` to
verify where a profile's actor, executors, and commands will run before
starting.

## Commands

Rules apply in order. A single-valued action takes its first assigned value.
Collections accumulate until `final: true` stops evaluation. A command with no
placement uses `default_executor`, or stays in the actor when none is set.

```yaml
commands:
  - { match: "{bash,sh,zsh} **", to: build }            # glob over the command line
  - { match: "git push **", deny: "no pushes from the agent" }
  - match: "pytest **"
    when: { cwd: "~/workspace/**", tty: false }
    to: build
    timeout: 10m
    env: { set: { CI: "1" } }
```

## Workspace

The directory the work operates on is one setting. The driving program starts in
it.

```yaml
actor:
  workspace:
    path: ~/project                      # in the actor
    from: executor:build:~/project       # live from an executor; or host:PATH (copied in), or omit it
    writable: true
```

`from: executor:NAME:PATH` mounts the executor's directory at `path` and maps
`path` to `PATH` for the commands routed there. `~` is the profile user's home
in the actor, the control host's home on `host:`, and the executor's own home on
an executor side.

Without `actor.workspace`, the workspace is the folder `needy run create` runs
in, mounted live at `~/workspace`, when the executor that runs the commands
(`default_executor`, else the only one) can reach it: a native executor of the
creating client (`runner: { client: true }`), or a native or `linux-container`
executor on the control host when the CLI runs there too (it reaches the daemon
over a Unix socket). Any other executor fails the create and asks for
`workspace.from`. A profile without executors keeps the actor's home.
`run create` prints where the workspace is.

## Files

Named mappings move other files between the control host (`host:`), the actor
(`actor:`), and an executor (`executor:NAME:`). `run_files` lists the ones every
run uses; a rule's `files:` adds some for the commands it matches.

```yaml
files:
  login-in:                        # copied in when the run starts
    mode: copy-in
    from: "host:~/needy-state/opencode/"
    to: "actor:~/.local/share/opencode/"
    filter: ["- /cache/**", "+ **"]
  login-out:                       # copied back when the run ends (not on `run stop --now`)
    mode: mirror-out
    from: "actor:~/.local/share/opencode/"
    to: "host:~/needy-state/opencode/"
run_files: [login-in, login-out]
```

A `mode: sync` mapping (`from: "host:STORE"`, `to: "actor:PATH"`) shares state
across concurrent runs and survives `run stop --now`: each run works on its own
copy of the store and merges back three-way when it ends, with an `on-conflict`
map (`store`, `run`, `lines`) and pluggable merge drivers (such as OpenCode's
SQLite row-merge driver). Dev Container features can declare sync mappings under
`customizations.needy.files` so agent profiles sync state automatically. See
[Sync mapping](../TERMS_AND_CONCEPTS.md#sync-mapping).

## Network

The actor has no network of its own: everything goes through the gateway on the
control host. HTTPS is tunnelled as is (no MITM, no injected CA, and so no
injected headers).

```yaml
actor:
  network:
    allow: [opencode.ai, "*.opencode.ai"]   # `*.x` also matches `x`
    services:
      ollama:                               # in the actor: http://ollama.needy
        url: http://127.0.0.1:11434         # resolved on the control host
```

A service is a plain-HTTP name the actor uses; the gateway forwards it to `url`,
so the actor never learns the real address. `needy config profile check` lists
the services and warns when a URL does not answer from the control host. The
gateway blocks loopback/link-local SSRF targets by default, accepts additional
`--gateway-block-cidr` rules, and can serve remote actor runners over an
authenticated TCP listener (`--gateway-listen`).

## Chaos

Named faults, each in one domain, with real effects. They fire from a seed, so a
replayed run injects the same ones; a fault without `probability`, `nth`,
`times`, `after`, or `until` fires on every match.

```yaml
chaos:
  seed: 1
  faults:
    curl-down:
      exec: "curl **"                   # the command exits 7 without running
      exit: 7                           # or signal: SIGSEGV, hang: true, delay: 2s
      stderr: "curl: (7) Failed to connect\n"
      times: 3
    flaky-json:
      file: { open: "**/*.json" }
      errno: EIO                        # ENOENT, EACCES, EPERM, EOPNOTSUPP or EIO
      probability: 0.1
    slow-example:
      net: { connect: "*.example.com" }
      delay: 2s                         # or refuse, reset, hang; status: 503 with net: { request: ... }
    overheat:
      sensor: { temp_c: 101 }
      at: 30s
```

Change a live run's faults; the trace records each change:

```sh
needy run chaos add api-down --request ollama.needy --status 503 --after 1m
needy run chaos ls
needy run chaos rm api-down
```

## Executors

```yaml
executors:
  build:
    driver: linux-container        # or native
    runner: { labels: [kvm] }      # where; the control host when absent
    key: "{run}"                   # commands with the same key share an instance
    max_procs: 8
    container:                     # typed engine options
      image: needy-executor:latest
      cpus: 2
      memory: 4GiB
      env: { LANG: C.UTF-8 }
  mac: { runner: mac, path_map: { ~/tools: ~/needy-tools } }   # values: the runner's home
```

Privileged options (`privileged`, `cap_add`, `devices`, `gpus`, host mounts
outside the workspace) need the operator. Containers needy creates carry the
labels `needy.run` and `needy.role` (`actor` or `executor`):
`docker ps --filter label=needy.run=RUN` lists a run's containers.

## Actor images, recipes, and `builtin:`

```yaml
actor:
  driver: linux-firecracker
  recipe: builtin:gce-ubuntu-22.04   # or a path to your own recipe
  seed: worker-1                     # optional: stable SMBIOS, MAC, and /etc/machine-id
  image:
    root: ubuntu:24.04               # optional; defaults to ubuntu:24.04
    pull_policy: if-not-present      # if-not-present (default), always, or never
    layers:
      - feature: ghcr.io/needy-dev/features/opencode:1  # OCI, https, local path, or builtin:sshfs
      - run: apt-get update && apt-get install -y jq
    env: { OPENCODE_DISABLE_AUTOUPDATE: "1" }
    hooks:
      start: ["echo ready"]
      stop: ["echo bye"]
  command: [opencode]
user: { name: bob, uid: 1001, gid: 1001 }   # whoami, id, HOME and getent agree in the actor
```

- **Image definitions** (inline under `actor.image` or in a separate `.yaml`
  file) layer Dev Container `feature:` entries and `run:` commands over a
  `root:` OCI image, with optional `pull_policy:`, `env:`, and `hooks:` (`start`
  / `stop`). Layers build and cache automatically on `needy run create` or
  `needy image build` (no repo checkout needed), and a sibling `.lock` file pins
  OCI feature digests and interactive commits from `needy image edit`:
  ```sh
  needy image edit --profile builtin:opencode
  needy image log --profile builtin:opencode
  needy image rollback --profile builtin:opencode
  needy image export --profile builtin:opencode -o opencode.tar
  needy image prune
  ```
- **Sharing built layers**: `needy daemon serve --layer-registry REPOSITORY`
  (or `needy config set daemon.layer_registry REPOSITORY`) shares built steps
  through an OCI registry repository such as
  `registry.example.com/needy/layers`. A step this daemon has not built is
  pulled from the registry before it is built, and a built step is pushed
  there as an ordinary OCI image tagged `key-<cache key>`: one gzip layer, a
  standard image config, and needy's metadata in `dev.needy.image.*`
  manifest annotations, so `crane`, `skopeo` or any registry can copy and keep
  it. An `http://` prefix opts into a plain-HTTP registry, which never gets
  credentials. A failed lookup, pull or push is logged and traced
  (`image.layer.pull`, `image.layer.push`) without failing the build.
- **Registry credentials and TLS** (feature fetches and the layer registry):
  - Credentials come from the Docker credential store: `$DOCKER_CONFIG/config.json`
    (default `~/.docker/config.json`), its `auths`, `credHelpers` and
    `credsStore`, so `docker login` and helpers such as
    `docker-credential-ecr-login` work. An `identitytoken` (ACR, Docker Hub
    SSO) is exchanged at the registry's token service for an access token;
    it is never sent to the registry itself. Each process (control daemon,
    runner) reads its own store; credentials are never forwarded.
  - TLS trusts the system's store (and `SSL_CERT_FILE` / `SSL_CERT_DIR`) and
    the CA files of the registry's host in containers-style `certs.d`:
    `$XDG_CONFIG_HOME/containers/certs.d/HOST[:PORT]/*.crt` (default
    `~/.config/containers/certs.d`), then `/etc/containers/certs.d`, then
    `/etc/docker/certs.d`. A host with a port may also be spelled
    `HOST_PORT_` (containerd's spelling, tried first) and is `HOSTPORT` on
    Windows. The first directory that exists for the host is
    used. Client certificates (`*.cert`, `*.key`) are not presented yet.
  - `needy daemon serve --insecure-registry HOST[:PORT]` (repeatable; or
    `daemon.insecure_registries: [HOST]` in the settings file) stops verifying
    that host's TLS certificate, and logs a warning on every connection. It
    does not switch to plain HTTP and still sends credentials; prefer putting
    the registry's CA in `certs.d`.
- **Prebuilt OCI references**: `actor.image: REF` runs an image `FROM` needy's
  base on every driver (`needy image build --from REF` for VM drivers; see
  [`examples/custom-image`](../../examples/custom-image/Dockerfile)).
- **Recipes & stable identity**: recipes set CPU model and count, memory,
  kernel, firmware strings, devices, hostname (`instance-{time}`),
  `os.machine_id` (`/etc/machine-id`), and `required:` identity fields. Setting
  `actor.seed` derives per-machine values deterministically across runs instead
  of from each run ID. `needy image build --recipe RECIPE` builds a kernel with
  a recipe's kernel options (needs a repo checkout).
- **What is `builtin:`**: needy keeps its embedded catalog small — two recipes
  (`builtin:gce-ubuntu-22.04`, `builtin:dell-optiplex-ubuntu-24.04`), one
  feature (`builtin:sshfs`, auto-prepended for custom roots), and four minimal
  agent profiles (`builtin:opencode`, `builtin:codex`, `builtin:pi`,
  `builtin:herdr`) that pull their features from `ghcr.io/needy-dev/features/*`.

## Environment and data

| Variable | Meaning |
| --- | --- |
| `NEEDY_STATE_DIR` | traces, `state.db`, run directories, the socket on macOS (`$XDG_STATE_HOME/needy`) |
| `NEEDY_DATA_DIR` | VM images in `images/<driver>/`, layer store, locks, and guest disks (`$XDG_DATA_HOME/needy`) |
| `NEEDY_CONFIG_DIR` | the global configuration, saved connections, and `profiles/` (`$XDG_CONFIG_HOME/needy`) |
| `NEEDY_RUNTIME_DIR` | the daemon's socket (`$XDG_RUNTIME_DIR/needy`) |
| `NEEDY_REPO` | the checkout `needy image build` builds base images from, when not run inside it |
| `DOCKER_HOST` | the Docker-compatible engine (Docker Engine, OrbStack, Podman) |

`--config FILE` selects the client configuration file, `--context NAME` selects
a saved connection for one invocation, and `-v` turns on logs.
