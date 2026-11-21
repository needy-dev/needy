# Coding agents in an actor

Four coding agents packaged as Dev Container features under [`features/`](../../features/),
with one profile per agent. Each profile declares an inline `actor.image`
definition using needy's built-in feature references (`builtin:<agent>`), so
`needy run create` builds and caches the feature layers automatically without a
separate `docker build` step.

The agent runs in the actor. Every shell it starts goes to the `build`
executor, which holds the workspace, so all the commands the agent runs go
there too.

| Profile | Feature | Default version | Synced state directory |
| --- | --- | --- | --- |
| [`codex.yaml`](codex.yaml) | `builtin:codex` ([`features/codex/`](../../features/codex/)) | 0.167.2 | `~/.codex` |
| [`pi.yaml`](pi.yaml) | `builtin:pi` ([`features/pi/`](../../features/pi/)) | 1.5.2 (Node.js 26.13.1) | `~/.pi/agent` |
| [`opencode.yaml`](opencode.yaml) | `builtin:opencode` ([`features/opencode/`](../../features/opencode/)) | 1.19.5 | `~/.local/share/opencode` |
| [`herdr.yaml`](herdr.yaml) | `builtin:herdr` ([`features/herdr/`](../../features/herdr/)) + the three above | 0.9.5 | all three above |

Each feature's `install.sh` pins its default download to one release and checks
its checksum (with provenance comments). Passing `with: { version: "..." }` on
the layer installs another release.

## Start an agent

Start any profile directly; needy pulls the base image if missing, builds each
feature layer once in a disposable session, and caches it by content key:

```sh
needy run create -it --profile examples/agents/codex.yaml
```

To stack an agent feature on a custom OCI root (for example
`debian:bookworm-slim`), add `root:` to `actor.image`; needy automatically
prepends `builtin:sshfs` as the first layer when the root is not needy's own
base image.

## Pick the driver

Each profile uses `linux-firecracker` and a `linux-container` executor. For
another driver, write a short profile that extends the agent's profile,
for example `~/.config/needy/profiles/codex.yaml`:

```yaml
# Linux without KVM
extends: /path/to/needy/examples/agents/codex.yaml
actor:
  driver: linux-container
  container: { oci_runtime: runc }
```

```yaml
# Apple silicon Mac: the executor is the Mac itself, as in examples/macos-vz.
# mkdir -p /tmp/needy-agents first.
extends: /path/to/needy/examples/agents/codex.yaml
actor:
  driver: macos-vz                       # or macos-apple-container
  workspace: { from: "executor:build:/tmp/needy-agents" }
executors:
  build: { driver: native }
```

On a Mac, the agent's commands run on the Mac as your user, in that
directory. On Linux they run in a container.

## First start and login

The actor cannot receive a browser's callback to its own `localhost`. Use
the sign-in flow that does not need it:

| Agent | Sign-in that works from an actor |
| --- | --- |
| Codex | **Sign in with Device Code** (or `codex login --device-auth`). Open `https://auth.openai.com/codex/device` on any machine and type the code. First, enable device code sign-in in the ChatGPT security settings. The default **Sign in with ChatGPT** waits for a callback on `localhost:1455` and does not finish. |
| pi | `/login`, then a provider. If a subscription's browser sign-in cannot call back, paste the final redirect URL or the code when pi asks. GitHub Copilot uses a device code. |
| opencode | `/connect` (or `opencode auth login`). Paste an API key, or use a device code for GitHub Copilot. A browser sign-in that calls back to `localhost` does not finish. |
| herdr | herdr needs no login. Log in to each agent in a pane, as above. |

An API key can replace the login. Each profile names the variable in an
`actor.env` comment (`OPENAI_API_KEY`, `OPENROUTER_API_KEY`, ...). Put it in
a private profile that extends the example, not in a shared file.

## Where credentials go

Each agent feature declares its state directory under
`customizations.needy.files` in `devcontainer-feature.json`. Needy
automatically creates a per-run copy from `<data_dir>/files/<agent>`, mounts it
into the actor, and three-way merges changes back into the store when the run
ends:

| Agent | Store (`<data_dir>/files/...`) | Actor |
| --- | --- | --- |
| Codex | `files/codex` | `~/.codex` |
| pi | `files/pi` | `~/.pi/agent` |
| opencode | `files/opencode` | `~/.local/share/opencode` |

To override the store location for a specific account or project, declare a
`mode: sync` mapping for the same actor path in your own profile.

Read these points before you log in:

- **The credentials are in the actor.** The agent, and any program it
  runs in the actor, can read them. needy cannot inject a credential into
  HTTPS requests yet (`TODO(mitm)`). An API key in `actor.env` is in the
  actor's environment, and also in the run's `profile.yaml` in needy's
  state directory.

## Find missing hosts

The actor can reach only the hosts in `actor.network.allow`. The gateway
refuses all other hosts with 403 and records each refusal:

```sh
needy run report latest
#   Findings
#     network denied: files.openai.com (actor network)
```

Add a host that the agent needs to `allow`, in your own profile. The
profiles leave out some hosts on purpose. The agents start and work
without them:

| Agent | Denied, and left out |
| --- | --- |
| Codex | `github.com`, `api.github.com`, `raw.githubusercontent.com`, `files.openai.com` |
| herdr | `herdr.dev` (version and agent-manifest checks) |

## herdr

[`herdr.yaml`](herdr.yaml) starts herdr with `builtin:codex`, `builtin:pi`,
`builtin:opencode`, and `builtin:herdr` stacked in one image. Its panes run
shells in the actor, so `codex`, `pi` and `opencode` start there. A shell that
one of these agents starts goes to the executor (a rule with `when.under`).

When herdr's client exits (ctrl+b q), the run ends. Use ctrl-] to detach
from needy and leave the run going.

## The workspace

The workspace starts empty, in the executor's `~/workspace`. The agent can
clone a repository there with its own tools. You can also copy files in
with `needy fs cp ./src build:/home/agent/workspace`. The `build`
executor belongs to the run (`key: "{run}"`). To keep the work on a
directory you choose, use the Mac's directory (above), or a `native`
executor with a `workspace:` of your own.
