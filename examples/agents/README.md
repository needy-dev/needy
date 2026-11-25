# Coding agents in an actor

Four coding agents are built into needy as ready-to-use profiles
(`builtin:codex`, `builtin:opencode`, `builtin:pi`, `builtin:herdr`) backed by
published OCI Dev Container features from
[`needy-dev/features`](https://github.com/needy-dev/features). No repository
clone or `docker build` step is needed: `needy run create` pulls the base image
and OCI features if missing, builds each feature layer once in a disposable
session, and caches it by content key.

The agent runs in the actor. Every shell it starts goes to the `build`
executor, which holds the workspace, so all the commands the agent runs go
there too.

| Built-in profile | Example file | Feature | Synced state directory |
| --- | --- | --- | --- |
| `builtin:codex` | [`codex.yaml`](codex.yaml) | `ghcr.io/needy-dev/features/codex:1` | `~/.codex` |
| `builtin:pi` | [`pi.yaml`](pi.yaml) | `ghcr.io/needy-dev/features/pi:1` | `~/.pi/agent` |
| `builtin:opencode` | [`opencode.yaml`](opencode.yaml) | `ghcr.io/needy-dev/features/opencode:1` | `~/.local/share/opencode` |
| `builtin:herdr` | [`herdr.yaml`](herdr.yaml) | `ghcr.io/needy-dev/features/herdr:1` + the three above | all three above |

Each feature's `install.sh` pins its default download to one release and checks
its checksum (with provenance comments). Passing `with: { version: "..." }` on
the layer installs another release.

## Start an agent

On Linux with KVM and Docker, start any built-in profile directly:

```sh
needy run create -it --profile builtin:opencode
# also: builtin:codex, builtin:pi, builtin:herdr
```

To stack an agent feature on a custom OCI root (for example
`debian:bookworm-slim`), add `root:` to `actor.image`; needy automatically
prepends `builtin:sshfs` as the first layer when the root is not needy's own
base image.

## Pick the driver

The built-in profiles stay minimal (`linux-firecracker`, the agent feature and
command, and `actor.network.allow: ["*"]`), with no pre-bound executor, routing
rules, or `actor.workspace`. The full example profiles in this directory
([`opencode.yaml`](opencode.yaml), [`codex.yaml`](codex.yaml),
[`pi.yaml`](pi.yaml), [`herdr.yaml`](herdr.yaml)) additionally add a
`linux-container` `build` executor, shell routing, and per-agent host
allowlists.

For another driver or a custom executor, write a short profile that extends
`builtin:<agent>` (no repo checkout required), for example
`~/.config/needy/profiles/opencode.yaml`:

```yaml
# Linux without KVM (requires SSHFS and FUSE 3 on the actor host)
extends: builtin:opencode
actor:
  driver: linux-container
  container: { oci_runtime: runc }
```

```yaml
# Apple silicon Mac: commands run on the Mac itself in the current directory
extends: builtin:opencode
actor:
  driver: macos-vz                       # or macos-apple-container
executors:
  mac: { driver: native }
commands:
  - { match: "**", to: mac }
unknown: mac
```

On a Mac with that profile, `needy run create` mounts your current directory
live at `~/workspace` and runs routed commands on the Mac as your user.

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
a private profile that `extends: builtin:<agent>`, not in a shared file.

## Where credentials go

Each agent feature declares its state directory under
`customizations.needy.files` in `devcontainer-feature.json`. Needy
automatically creates a per-run copy from `<data_dir>/files/<agent>`, mounts it
into the actor, and three-way merges changes back into the store when the run
ends (with a SQLite row-merge driver for OpenCode's database):

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

`builtin:herdr` ([`herdr.yaml`](herdr.yaml)) starts herdr with the `codex`,
`pi`, `opencode`, and `herdr` features from `ghcr.io/needy-dev/features`
stacked in one image. Its panes run shells in the actor, so `codex`, `pi` and
`opencode` start there. A shell that one of these agents starts goes to the
executor (a rule with `when.under`).

When herdr's client exits (ctrl+b q), the run ends. Use ctrl-] to detach
from needy and leave the run going.

## The workspace

The `builtin:*` profiles leave `actor.workspace` unset and declare no
executors. When you extend a built-in profile with a `native` (local or
`runner: { client: true }`) or local `linux-container` executor,
`needy run create` mounts the directory you run it in live at `~/workspace`.

The full example profiles in this directory (`examples/agents/*.yaml`) set
`actor.workspace` to an empty directory in the `build` container's
`~/workspace`. The agent can clone a repository there with its own tools, or
you can copy files in with `needy fs cp ./src build:/home/agent/workspace`.
