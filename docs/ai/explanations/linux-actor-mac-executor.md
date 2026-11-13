# Run a Linux program with Mac tools

Start bash, Claude Code, or OpenCode in a Linux container. Send captured child
processes to a Mac. Both machines see the same project files.

**Evidence level: live probe.** The entrypoint, Mac shell, shared directory,
and agent interfaces were exercised. See the
[onboarding review](../references/cli/2026-11-14-cli-configuration-and-access-spec.md#onboarding-review).
Fresh installation and authenticated model tool calls still need verification.
This walkthrough is a documentation draft, not a platform support statement.

## Choose an entrypoint

| Start with | What you can observe | Complete profile |
| --- | --- | --- |
| [bash](#bash) | Linux builtins and Mac child processes | [bash.yaml](../../../examples/linux-mac/bash.yaml) |
| [Claude Code](#claude-code) | Claude in Linux, tool commands on the Mac | [claude.yaml](../../../examples/linux-mac/claude.yaml) |
| [OpenCode](#opencode) | OpenCode in Linux, tool commands on the Mac | [opencode.yaml](../../../examples/linux-mac/opencode.yaml) |

Complete the common steps first. Then save the profile for your entrypoint.
The website reads these same profiles.

## Where things run

| Machine | Role | Files and terminals |
| --- | --- | --- |
| Linux server | Control daemon and container actor | One setup terminal. Agent state stays on this server. |
| Mac | Native executor runner | One tunnel terminal and one runner terminal. Project: `~/needy-demo`. |
| Inside the run | Your Linux entrypoint | The project appears at `/home/agent/workspace`. |

This walkthrough starts runs from the Linux server. The Mac does not need
client pairing for this task. Pairing authorizes a control client; a join
token registers a runner that executes commands.

If you already have a working setup, retain it. Check the runner name and OS,
then continue at [Save a profile](#save-a-profile).
These examples call the runner `mac`. If yours has another name, change
`executors.mac.runner` to that name. The executor key can remain `mac`.

## 1. Prepare the Linux server

Start with a Linux server that you can reach over SSH. Install
[Docker Engine](https://docs.docker.com/engine/install/) first.
Docker must work under the non-root user who will run needy.

In the **Linux setup terminal**, install the released needy binary:

```sh
curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
needy --version
```

For Ubuntu or Debian, install the workspace mount helper:

```sh
sudo apt-get update
sudo apt-get install -y sshfs
sudo sh -c 'grep -qxF user_allow_other /etc/fuse.conf || echo user_allow_other >> /etc/fuse.conf'
docker info --format '{{.ServerVersion}}'
test -r /dev/fuse && test -w /dev/fuse
```

For another distribution, install its equivalent `sshfs` package.
The last two commands must succeed before you continue.
No KVM or recipe is required for this container example.

## 2. Prepare the Mac

In the **Mac runner terminal**, install needy and create the project directory:

```sh
curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
needy --version
mkdir -p ~/needy-demo
```

The Mac needs your project tools, such as Xcode. It does not need Docker,
actor images, or a guest needy for this native runner.
If you want to use a tool, check its path in this same terminal:

```sh
command -v xcodebuild
command -v codex
```

The runner inherits this terminal's `PATH`. A background service can have a
different `PATH`.

## 3. Start the control daemon

In the **Linux setup terminal**, configure the network listener and install a user service:

```sh
needy config use local
needy config set daemon.listen '[tcp://127.0.0.1:7400]'
needy daemon service install --now
needy daemon service status
needy runner ls
```

The user systemd manager must be available. Local administration uses the
private management socket, so no bearer or address export is required.
If an owned service already exists, use `install --replace --now` to update it.
Replacement stops the service and active runs before applying the new snapshot.
For development without a service manager, run `needy daemon serve --listen tcp://127.0.0.1:7400` in a separate foreground terminal.

## 4. Open one tunnel

Open a **separate terminal on the Mac**.
Replace `user@example.com` with your server's SSH destination:

```sh
ssh -N -o ExitOnForwardFailure=yes -L 7400:127.0.0.1:7400 user@example.com
```

Keep this terminal open. SSH waits without a shell prompt.
Reuse this tunnel for all later connections. A second tunnel on local port
7400 will conflict with the first.

## 5. Register the Mac runner

In the **Linux setup terminal**, create a join token:

```sh
needy runner invite --ttl 1h --roles executor
```

Copy the printed invitation code. In the **Mac runner terminal**, enroll once:

```sh
needy --address tcp://127.0.0.1:7400 runner join mac --roles executor
```

Paste the code at the hidden prompt. The command saves the transport,
credential and role selection, then returns. Start the enrolled runner:

```sh
needy runner serve
```

Keep this process active outside the server's SSH session.

## 6. Check which machine joined

In the **Linux setup terminal**, run:

```sh
needy runner ls
needy proc exec -T -n --runner mac -- uname -s
```

Expect `mac` to be `online`, with `macos` in the HOST column.
The command must print `Darwin`. A runner name alone does not prove its location.

## Save a profile

Get the examples on the **Linux server**.
If you already have a checkout, enter its root instead:

```sh
git clone https://github.com/needy-dev/needy
cd needy
mkdir -p ~/.config/needy/profiles
```

Choose one entrypoint below. Each profile is complete and independent.
The actor workspace mounts the Mac project and derives its command path map.

### bash

Save the [bash profile](../../../examples/linux-mac/bash.yaml):

```sh
cp examples/linux-mac/bash.yaml ~/.config/needy/profiles/bash-mac.yaml
needy config profile check bash-mac --mode control --cmd 'bash -c "uname -s"'
needy run create -it --profile bash-mac
```

Inside the **new Linux bash**, run this single command line:

```sh
printf 'entrypoint=%s\n' "$OSTYPE"; bash -c 'uname -s; pwd'
```

Expect `entrypoint=linux-gnu`, then `Darwin`, then the Mac project path.
The entrypoint stays in Linux. The child bash runs on the Mac.

`cd`, `export`, `pwd`, and other builtins do not create an exec.
They stay in the current shell. An external `/bin/pwd` can therefore show a
different path from builtin `pwd`.
If a Mac-only tool does not exist in the actor, run it in the child shell:

```sh
bash -c 'xcodebuild -version'
```

### Claude Code

Build the agent image on the **Linux server**, from the checkout root:

```sh
needy_release=$(needy --version | awk '{print $2}')
docker build --build-arg BASE="ghcr.io/needy-dev/needy-actor:v${needy_release}" \
  -t needy-agents:latest examples/agents
mkdir -p ~/needy-state/claude-mac
chmod 700 ~/needy-state/claude-mac
cp examples/linux-mac/claude.yaml ~/.config/needy/profiles/claude-mac.yaml
needy config profile check claude-mac --mode control --cmd 'bash -c "uname -s"'
needy run create -it --profile claude-mac
```

Complete Claude's first-start prompts. For login, open the printed URL in
your browser and return the code when requested. You can use `/login` inside
Claude. See the [agent login guide](../../../examples/agents/README.md#first-start-and-login).

The Linux server keeps the mapped state in `~/needy-state/claude-mac/` after
normal shutdown.

### OpenCode

The released actor base already contains OpenCode.
In the **Linux setup terminal**, create its persistent directories and start it:

```sh
mkdir -p ~/needy-state/opencode-mac/{data,config,state}
chmod 700 ~/needy-state/opencode-mac ~/needy-state/opencode-mac/{data,config,state}
cp examples/linux-mac/opencode.yaml ~/.config/needy/profiles/opencode-mac.yaml
needy config profile check opencode-mac --mode control --cmd 'bash -c "uname -s"'
needy run create -it --profile opencode-mac
```

Inside **OpenCode**, enter `/connect`. Choose your model provider and complete
its login. Data, configuration, and interface state have separate mapped directories.

## Check an agent tool call

After login, send this prompt to Claude or OpenCode:

> Create needy-check.txt with the text "shared workspace check" using your
> file editor. Then run uname -s and cat needy-check.txt with your shell tool.
> Show the exact output.

Check the actual tool output. Expect `Darwin` and `shared workspace check`.
On the **Mac**, the file must also exist in `~/needy-demo`.
An agent's built-in file editor uses the mount; it is not an exec route.

## Return, stop, and start again

Press **Ctrl-]** to detach without stopping the run.
In the **Linux setup terminal**, list runs:

```sh
needy run ls
```

Replace `RUN_ID` with the displayed run ID. To return, run this command alone:

```sh
needy run attach RUN_ID
```

After detaching again, stop the run normally:

```sh
needy run stop RUN_ID
needy run report RUN_ID
```

Normal shutdown mirrors agent state to the Linux server.
`run stop --now` skips mirror-out, so new login state can be lost.
Start another run with the same profile to reuse its saved state.

If you restart the Mac runner, retain the tunnel and use the stored credential:

```sh
needy runner serve
```

## Boundaries and troubleshooting

| Symptom | Check |
| --- | --- |
| No local daemon discovered | On the server, inspect `needy daemon service status` and `needy daemon service logs`. |
| Revoked remote credential | Inspect the selected context with `needy config context check`. Runner credentials remain separate. |
| Runner offline or wrong OS | Mac tunnel, runner process, and the HOST column. |
| Mac tool not found | Tool installation and the runner terminal's `PATH`. |
| Wrong cwd or missing project files | `actor.workspace` and the Mac project directory. |
| Missing login after restart | Normal shutdown, server state directories, and file mappings. |
| Mac-only command absent in Linux | Start a routed `bash -c` and invoke the tool there. |

The wildcard covers captured execs. Container shims do not guarantee capture
of programs installed or built after the image build.
The profile allows HTTP/HTTPS hosts through the gateway.
It does not change the agent's own approval policy or the Mac user's permissions.
Native Mac commands run as the user who started the runner.

This example does not configure a machine recipe. An lxcfs warning concerns
container resource views; it is not evidence of recipe fidelity.
For other runtimes and policies, see the [README reference](../../../README.md#reference).
