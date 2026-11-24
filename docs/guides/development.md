# Development and Architecture

## Architecture

Dependencies point downward; a crate never depends on one above it.

| Crate | Role |
| --- | --- |
| `cli` | the `needy` binary; inside an actor, injected as the guest needy, also its init and exec proxy |
| `daemon` | the control plane: runs, pool, scheduling, routing, relay, files, runners, `state.db` |
| `core-actor` | actor drivers, the guest (init, capture, proxy), the gateway sidecar, identity |
| `core-executor` | executor drivers, the container engine client, the process host |
| `core` | interceptor domains (exec, net, file) and their chains, rules, chaos, network policy, recipes, events, reports; no IO |
| `devcontainer-features` | standalone OCI / HTTPS / local Dev Container feature resolver, cache, and dependency graph |
| `oci-layer` | standalone OCI image archive and layer tar manipulation |
| `trace` | the event pipeline and its sinks |
| `config` | profile, image definition, and recipe schemas, loading and validation |
| `api-proto`, `api-client`, `api-server` | the gRPC API and its two sides |

Extension points are directories with one implementation each: actor drivers
(`core-actor/src/driver/`), executor drivers (`core-executor/src/driver/`),
interceptor domains (`core/src/intercept/`), sidecars
(`core-actor/src/sidecar/`), and trace sinks (`trace/src/sink/`).

## Build, check, and test

```sh
make check
make test
make proto
cargo xtask bench --help      # latency and throughput probes
cargo xtask e2e               # the end-to-end matrix on this host
```

On macOS, run `ulimit -n 4096` before `make test` or `cargo test -p needy-daemon`;
the default 256 open-file limit is too low for the parallel daemon test suite.

## End-to-end matrix (`cargo xtask e2e`)

`cargo xtask e2e` runs every cell of the end-to-end matrix this host can run:
each actor driver it has, with each terminal mode, way a run ends, file mapping,
fault domain, network rule, and user, each cell against its own non-root control
daemon, checked for invariants (ownership and modes, mirror-out after every
ending, placement in the trace, private sockets, nothing left behind). It builds
the guest needy and the images from the tree first.

A cell it cannot run here is reported as not covered, with the reason, and the
exit code says so. `--list` shows the cells, `--only` and `--skip` pick some,
`--remote HOST` runs on another machine from `git archive HEAD`, and
`--hosts FILE` runs on several at once, each building natively.

GitHub Actions runs the checks and tests on Linux, macOS, and Windows, the e2e
cells without an actor on Linux and macOS, and builds the release assets.

## Animated README figures (`apps/docs`)

The figures in the README are animated scenes in [`apps/docs`](../../apps/docs),
a page you can also open to step through them and route your own commands:

```sh
cd apps/docs && pnpm install
pnpm dev                      # the page
pnpm banners                  # re-render docs/assets/banners (needs Chrome and img2webp)
```
