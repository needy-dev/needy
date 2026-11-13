# Linux program, Mac tools

Follow the [walkthrough](../../docs/ai/explanations/linux-actor-mac-executor.md)
before starting these profiles. The control daemon runs on Linux. The runner
named `mac` runs on the Mac. The project lives in the Mac's `~/needy-demo`.

| Profile | Linux entrypoint | Captured child processes |
| --- | --- | --- |
| [bash.yaml](bash.yaml) | bash | Mac |
| [claude.yaml](claude.yaml) | Claude Code | Mac |
| [opencode.yaml](opencode.yaml) | OpenCode | Mac |

The website reads these files directly. `actor.workspace` derives both the
live mount and the command path map. Agent state is copied in from the Linux
server and mirrored back after normal shutdown.

Container capture uses image shims. New unshimmed binaries are outside this
capture. Shell builtins stay in their shell. Mac-only tools can run inside
a routed `bash -c`.
