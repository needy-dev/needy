# Coding agents in an actor

One actor image with five coding agents, and one profile for each agent. The
agent runs in the actor. Every shell it starts goes to the `build` executor,
which holds the workspace, so all the commands the agent runs go there too.

| Profile | Agent | Version in the image |
| --- | --- | --- |
| [`claude.yaml`](claude.yaml) | Claude Code (native build) | 2.1.311 |
| [`codex.yaml`](codex.yaml) | Codex (static musl package) | 0.167.2 |
| [`pi.yaml`](pi.yaml) | pi (`@earendil-works/pi-coding-agent`, on Node.js 26.13.1) | 1.5.2 |
| [`opencode.yaml`](opencode.yaml) | opencode (in needy's base image) | 1.19.5 |
| [`herdr.yaml`](herdr.yaml) | herdr, which runs the other four in panes | 0.9.5 |

The [`Dockerfile`](Dockerfile) pins each download to one release and
checks its checksum. Each pin has a comment that names its source.

## Build the image

Build the image on the host where its actors run, so that it has the
host's architecture. The image is `FROM` needy's base image:
`needy-actor:latest` from `needy image build --linux-container`, or
the released base for your needy:

```sh
docker build -t needy-agents examples/agents
# or, without a needy checkout:
docker build --build-arg BASE=ghcr.io/needy-dev/needy-actor:v<version> -t needy-agents examples/agents
```

Then do the step for your driver:

| Driver | Step |
| --- | --- |
| `linux-container` | None. The engine runs `needy-agents`. |
| `linux-firecracker`, `macos-vz` | `needy image pull` once for the base VM image. Then run `needy image build --from needy-agents`, and run it again each time you rebuild the image. |
| `macos-apple-container` | `docker save needy-agents -o needy-agents.tar && container image load --input needy-agents.tar` |

## Pick the driver

Each profile uses `linux-firecracker` and a `linux-container` executor. For
another driver, write a short profile that extends the agent's profile,
for example `~/.config/needy/profiles/claude.yaml`:

```yaml
# Linux without KVM
extends: /path/to/needy/examples/agents/claude.yaml
actor:
  driver: linux-container
  container: { oci_runtime: runc }
```

```yaml
# Apple silicon Mac: the executor is the Mac itself, as in examples/macos-vz.
# mkdir -p /tmp/needy-agents first.
extends: /path/to/needy/examples/agents/claude.yaml
actor:
  driver: macos-vz                       # or macos-apple-container
  workspace: { from: "executor:build:/tmp/needy-agents" }
executors:
  build: { driver: native }
```

On a Mac, the agent's commands run on the Mac as your user, in that
directory. On Linux they run in a container.

## First start and login

Make the directory that keeps the agent's login and settings, then start
the agent:

```sh
mkdir -p ~/needy-state/claude
needy run create -it --profile examples/agents/claude.yaml
```

The actor cannot receive a browser's callback to its own `localhost`. Use
the sign-in flow that does not need it:

| Agent | Sign-in that works from an actor |
| --- | --- |
| Claude Code | `/login`, then a subscription or Console account. Open the URL it shows on any machine. Then paste the code that the page shows at `Paste code here if prompted`. `claude setup-token` on another machine gives a token for `CLAUDE_CODE_OAUTH_TOKEN`. |
| Codex | **Sign in with Device Code** (or `codex login --device-auth`). Open `https://auth.openai.com/codex/device` on any machine and type the code. First, enable device code sign-in in the ChatGPT security settings. The default **Sign in with ChatGPT** waits for a callback on `localhost:1455` and does not finish. |
| pi | `/login`, then a provider. If a subscription's browser sign-in cannot call back, paste the final redirect URL or the code when pi asks. GitHub Copilot uses a device code. |
| opencode | `/connect` (or `opencode auth login`). Paste an API key, or use a device code for GitHub Copilot. A browser sign-in that calls back to `localhost` does not finish. |
| herdr | herdr needs no login. Log in to each agent in a pane, as above. |

An API key can replace the login. Each profile names the variable in an
`actor.env` comment (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, ...). Put it in
a private profile that extends the example, not in a shared file.

## Where credentials go

When the run starts, needy copies the host directory into the actor. When
the agent exits, or `needy run stop` ends it, needy mirrors the directory
back to the host:

| Agent | Host | Actor |
| --- | --- | --- |
| Claude Code | `~/needy-state/claude/` | `~/.claude/` (`CLAUDE_CONFIG_DIR`, so `.claude.json` is there too) |
| Codex | `~/needy-state/codex/` | `~/.codex/` |
| pi | `~/needy-state/pi/` | `~/.pi/agent/` |
| opencode | `~/needy-state/opencode/` | `~/.local/share/opencode/` |

Read these points before you log in:

- `needy run stop --now` kills the actor without its mirror-out, so a
  login made in that run is lost.
- **The credentials are in the actor.** The agent, and any program it
  runs in the actor, can read them. needy cannot inject a credential into
  HTTPS requests yet (`TODO(mitm)`). An API key in `actor.env` is in the
  actor's environment, and also in the run's `profile.yaml` in needy's
  state directory.
- Deleting a file in the actor does not delete it on the host (`delete:
  false`). To log out, delete the host file as well.

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
| Claude Code | None, when `DISABLE_UPDATES` and `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL` are set. Without them: `downloads.claude.ai` and `github.com`. |
| Codex | `github.com`, `api.github.com`, `raw.githubusercontent.com`, `files.openai.com` |
| herdr | `herdr.dev` (version and agent-manifest checks) |

The profiles include the API and sign-in hosts. The tests did not log in
(see the verification in the
[actor images design](../../docs/ai/references/architecture/2026-11-12-actor-images-design.md#step-3-checks)),
so a host that only an authenticated session uses can be missing. If so,
`needy run report` shows it.

## herdr

[`herdr.yaml`](herdr.yaml) starts herdr. Its panes run shells in the
actor, so `claude`, `codex`, `pi` and `opencode` start there. A shell that
one of these agents starts goes to the executor (a rule with `when.under`).
The profile repeats the other profiles' environment, hosts and credential
mappings, because `extends` takes only one parent, and a child's list
replaces its parent's list.

When herdr's client exits (ctrl+b q), the run ends. Use ctrl-] to detach
from needy and leave the run going.

## The workspace

The workspace starts empty, in the executor's `~/workspace`. The agent can
clone a repository there with its own tools. You can also copy files in
with `needy fs cp ./src build:/home/agent/workspace`. The `build`
executor belongs to the run (`key: "{run}"`). To keep the work on a
directory you choose, use the Mac's directory (above), or a `native`
executor with a `workspace:` of your own.
