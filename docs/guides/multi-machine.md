# Multi-Machine and Remote Runner Setup

To run an actor on a Linux host and route child commands to a Mac (or another
Linux/Windows machine) over an SSH tunnel or Cloudflare Access TCP tunnel, see
the step-by-step
[Linux actor → Mac executor walkthrough](../ai/explanations/linux-actor-mac-executor.md).

## Enrolling runners and pairing remote clients

```sh
# On the control host: enable a loopback TCP listener and replace the service
needy config set daemon.listen '[tcp://127.0.0.1:7400]'
needy daemon service install --replace --now

# Enroll an executor runner (paste the code from `needy runner invite --roles executor`)
needy --address tcp://127.0.0.1:7400 runner join mac --roles executor
needy runner serve

# Or enroll a remote Firecracker actor runner (with `--gateway-listen` on the daemon)
needy --address tcp://127.0.0.1:7400 runner join kvm-1 --roles actor
needy runner serve --roles actor --gateway tcp://127.0.0.1:7401   # add --ephemeral for one-shot workers

# Or pair a remote control client (paste the code from `needy client invite --name mac-cli`)
needy --address tcp://127.0.0.1:7400 config context pair control
needy config use control
```

## Tunnels and proxies

Connector argv can carry connections through an authenticated tunnel directly
during pairing or enrollment (for example
`-- ssh -W 127.0.0.1:7400 user@example.com` or
`-- cloudflared access tcp --hostname needy.example.com`). Use a TCP route;
proxied HTTPS hostnames do not carry gRPC through Cloudflare Tunnel. Saved
contexts also support custom request headers for reverse proxies.

## Automated and multi-tenant deployments

`needy daemon serve` and `needy runner serve` expose additional controls for
ephemeral workers, pre-provisioned credentials, and host-side boundaries:

- **Static / TTL runner credentials**: pass `--runners-file PATH` to
  `needy daemon serve` (hot-reloaded) and `--credential-env VAR` or
  `--credential-file PATH` to `needy runner serve` (read-only; never writes
  `runner.json`).
- **Ephemeral runners & waiting**: pass `--ephemeral` to `needy runner serve`
  to serve one run and exit cleanly, and `--wait-for-runner DURATION` to
  `needy run create` to wait for a matching runner to connect.
- **Remote gateway & SSRF policy**: a remote actor runner runs its actor's
  gateway itself, so the actor's traffic leaves from the runner's host, under
  the profile's allowlist and the daemon's repeatable `--gateway-block-cidr
  CIDR` flags (which block outbound CIDR ranges in addition to the default
  loopback/link-local protections, and which the daemon sends to its runners).
  A runner older than this behaviour, or one talking to an older daemon,
  proxies to the daemon's authenticated TCP gateway instead: pass
  `--gateway-listen tcp://HOST:PORT` to the daemon and `--gateway` to the
  runner for those. See
  [the runner gateway design](../ai/references/architecture/2026-11-26-runner-gateway-design.md).
- **Per-actor upstream proxy** (from the release after v0.0.18, not
  released): pass `--gateway-upstream socks5h://HOST[:PORT]` (or
  `socks5://`, where the runner resolves and checks names itself) and
  `--gateway-upstream-credentials-file PATH` (`USERNAME:PASSWORD`, not
  readable by other users; `0440` with a Pod's `fsGroup` works) to
  `needy runner serve`, so the actor's traffic leaves through that proxy;
  `needy daemon serve` takes the same flags for the gateways it runs. The
  file is re-read per connection, a failing proxy gives `502` (never a
  direct connection), services bypass the proxy, and with `socks5h` the
  proxy must block private ranges itself. See "Upstream proxy" in the
  runner gateway design.
- **Shared image layers**: pass `--layer-registry REPOSITORY` (for example
  `registry.example.com/needy/layers`, or `http://HOST:PORT/PATH` for a
  plain-HTTP in-cluster registry) to `needy daemon serve` so control hosts
  that share the repository pull each other's built image steps instead of
  building them again. Credentials, private CAs (`certs.d`) and
  `--insecure-registry HOST` follow
  [registry credentials and TLS](configuration.md#actor-images-recipes-and-builtin);
  runners do not contact registries yet.
- **Host-file boundaries**: pass `--disable-host-files` to reject all `host:`
  file mappings, or `--host-files-root DIR` to confine `host:` mappings to an
  allowed directory tree.
