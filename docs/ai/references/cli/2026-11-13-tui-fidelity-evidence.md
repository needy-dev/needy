# TUI fidelity: evidence

Status: verified, 2026-11-13. Bug fixes G11–G14 of
[Terminals, attach and the interactive actor](2026-11-10-terminal-attach-design.md);
G15 and G16 stay open on purpose.

The question: do full-screen agent TUIs (opencode, pi, Claude Code, Codex,
herdr) behave through needy as they do on a local terminal? The answer
comes from a harness that plays the outer terminal, runs each TUI directly
and through each needy path, and compares what a person would see.

Evidence level: live probes of the agents image (`examples/agents`, the
versions its README lists) through the harness below, on the hosts named in
[Platforms](#platforms). No account was logged in: each TUI ran its
pre-login screens. This is not a claim about every terminal emulator.

## The harness

`cargo xtask tui` (`xtask/src/tui/`) drives a program on a pseudo-terminal
from a scenario and writes a JSON report:

- `tui run --scenario FILE -- COMMAND...` runs COMMAND on a PTY of 30 by
  100 cells with `TERM=xterm-256color` and `COLORTERM=truecolor`. The
  harness is the outer terminal: a `vt100` screen plus a `vte` scanner that
  answers the queries a modern terminal answers (DA1, DA2, DA3, XTVERSION,
  DECRQM, DSR 5 and 6, DECXCPR, the kitty keyboard flags, OSC 4/10/11/12,
  XTWINOPS 14/16/18, XTGETTCAP, DECRQSS), or none with `--answers none`.
  It encodes keys for the program's modes (application cursor keys, the
  kitty keyboard flags), brackets a paste only when the program asked,
  encodes mouse events for the program's mouse mode, and resizes.
- Scenario steps: `wait`, `gone`, `quiet`, `type`, `send`, `key`, `paste`,
  `mouse`, `focus`, `resize`, `snap`, `await`, `sleep`, `exit` (the module
  documentation lists them). `--marks` and `--follow` let two sessions
  snap the same moments, so a read-only viewer is compared with the client
  that types.
- The report holds each snap (text, cursor, 24-bit and indexed color cells,
  wide cells, title and modes), every query with whether it was answered,
  the exit, and the modes the program left on: alternate screen, hidden
  cursor, bracketed paste, mouse, application cursor keys, focus events,
  kitty keyboard flags, synchronized output, color scheme reports (2031),
  SGR pixel mouse (1016), autowrap off, modifyOtherKeys, cursor shape and
  cursor color.
- `tui diff A B` compares two reports with digits and long hexadecimal IDs
  masked; `tui replay RAW` reports the modes a raw capture leaves on.

`xtask/tui/matrix.sh OUT [TUI...]` runs each TUI's scenario
(`xtask/tui/TUI.scenario`) on six paths and diffs each against `direct`:

| Path | What runs |
| --- | --- |
| `direct` | `xtask/tui/direct.sh`: `docker run -it --init` of the image, as its user, with the profile's environment and arguments: the baseline |
| `create` | `needy run create -it` |
| `detach` | `needy run create -it`, then ctrl-] (the report shows what the detach left on) |
| `attach` | `needy run attach` of that run |
| `viewer` | `needy run attach --read-only` beside `attach`, snapping the same moments |
| `background` | `needy run create -t`, then `needy run attach`: no client terminal while the TUI starts |
| `exec` | `needy exec -t` of `direct.sh` on a native executor of the run |

```sh
make build && make guest && cargo build -p xtask
docker build -t needy-agents examples/agents
needy image build --from needy-agents          # VM drivers
needy daemon --mode control &
DRIVER=macos-vz xtask/tui/matrix.sh .tmp/tui/out          # or linux-firecracker, ...
```

The profiles the matrix writes extend `examples/agents/TUI.yaml`, start
each run from an empty login directory and mirror it out under
`OUT/state`, so no real login is read or written. The harness's own tests
(`cargo test -p xtask tui`) run in the ordinary suite.

## What herdr does that matters

herdr was read at
[`ee5bfb50`](https://github.com/herdrdev/herdr/tree/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde).
Its panes are modeled by a vendored libghostty-vt.

- **Environment.** Each pane gets `TERM=xterm-256color`,
  `COLORTERM=truecolor` and `TERM_PROGRAM=herdr`, and loses other
  multiplexers' variables
  ([`src/pane.rs#L100-L136`](https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/pane.rs#L100-L136)).
  The PTY is portable-pty's with the kernel's default termios.
- **Queries are answered by the pane's model, never forwarded.** DA1 is
  `CSI ? 62 ; 22 c`
  ([`device_attributes.zig#L37-L74`](https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/vendor/libghostty-vt/src/terminal/device_attributes.zig#L37-L74)),
  DECRQM, the kitty flags, DSR 5 and 6 and XTWINOPS come from the model,
  and OSC 10/11/4 from colors herdr asked the outer terminal for once.
- **Input is encoded again per pane** from the pane's modes (DECCKM, kitty
  flags, modifyOtherKeys, mouse mode and encoding); a paste is bracketed
  only if the pane asked; focus events only if it asked.
- **Synchronized output** holds rendering while 2026 is set; OSC 52 writes
  are decoded and forwarded to the foreground client; OSC 8 links are
  re-emitted without `id=`; titles are kept, not forwarded.
- **Restore** pops the kitty flags, turns off modifyOtherKeys, focus,
  bracketed paste, every mouse mode including 1016 and 9, color scheme
  reports (2031), turns line wrap on, shows the cursor and resets its
  shape
  ([`terminal_setup.rs#L683-L727`](https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/client/terminal_setup.rs#L683-L727),
  [`#L403-L415`](https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/client/terminal_setup.rs#L403-L415),
  [`terminal_modes.rs#L3-L4`](https://github.com/herdrdev/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/terminal_modes.rs#L3-L4)).
- **No job control**: no `WUNTRACED`, no SIGCONT; ctrl-z is a key for the
  pane, whose shell does job control.
- **Widths** come from libghostty's tables with grapheme clustering (2027)
  on; herdr blanks a cell whose width disagrees with `unicode-width`.

needy differs by design in one place: it passes the client's bytes through
and lets the client's terminal answer queries, so a program gets the real
terminal's answers while a client is attached. It adopts herdr's
environment (G11), restore (G12) and model-answered DA1 (G14).

## Results

Before: needy and its guest built from `5f64bac` (`main` before these
fixes); after: with G11–G14. Both on `macos-vz`, the whole matrix, with the same
harness. "Equal" means `tui diff` against `direct` found no difference in
text, cursor, colors, wide cells or modes at any snap.

| TUI | Behavior | Before | After |
| --- | --- | --- | --- |
| all five | typing, wide characters (中文) and emoji (🙂) | equal text, cursor and wide cells | equal |
| all five | Enter, arrows, Home/End, Escape, alt-b, ctrl-c | equal | equal |
| all five | bracketed paste of three lines | equal: bracketed exactly when the TUI asked | equal |
| opencode, pi, herdr | mouse click and wheel (they turn SGR any-motion on) | delivered, equal | equal |
| all five | resize 30x100 → 40x120 → 20x70 (`create`, `attach`) | equal | equal |
| all five | terminal queries with a client attached | all answered by the client's terminal, through needy; none lost | same |
| pi, opencode, Claude Code | 24-bit color | none: 256 colors, Claude Code also picks another default theme | equal (G11) |
| Codex | 24-bit color | its 5 tinted cells drawn in 256 colors | equal (G11) |
| herdr | start, typing, paste in a pane | equal but for its pane shell's prompt and the title (hostname): the actor's `.bashrc`, not needy | same |
| all five | exit and the terminal after it | nothing left on (`left_on` empty); Claude Code 2.1.311 does not exit on ctrl-c ctrl-c on its first-start screen in any path, `direct` included | same |
| Claude Code | detach (ctrl-]): shell after it | modifyOtherKeys and color scheme reports left on | nothing left (G12) |
| opencode | detach | color scheme reports, cursor shape (blinking block) and cursor color left | nothing left (G12) |
| pi, herdr | detach | color scheme reports and autowrap off left | nothing left (G12) |
| Codex | detach | nothing left | same |
| Claude Code | `run attach` redraw | modifyOtherKeys and color scheme reports missing | equal (G13) |
| opencode | `run attach` redraw | color scheme reports and cursor color missing | equal (G13) |
| pi, herdr | `run attach` redraw | color scheme reports and autowrap off missing | equal (G13) |
| all five | read-only viewer | as `attach`, until the typing client resizes: the viewer keeps its own size, by design (G6) | same |
| all five | `background` (no client terminal at start) | the queries about the terminal unanswered: no kitty keyboard (Claude Code, opencode), modifyOtherKeys instead (pi, opencode), no focus events (opencode), default colors (Codex) | DA1 answered (G14); the rest unchanged (G15) |
| all five | `exec -t` | equal | equal |
| pi, opencode | ctrl-z | the TUI leaves its screen and stays suspended | same (G16) |
| Claude Code, Codex | ctrl-z | nothing happens (in a shell they suspend) | same (G16) |
| herdr | ctrl-z | a key for the pane, as directly | same |

Read every TUI's queries in the reports: opencode asks the most (DA1,
XTVERSION, XTGETTCAP, DECRQM 1004/1016/2004/2026/2027/2031, the kitty
flags, OSC 4/10/11/12, CPR, XTWINOPS 14), herdr asks OSC 4 for all 256
colors, Claude Code asks DA1, XTVERSION, the kitty flags, DECRQM 2026/1016
and XTWINOPS 16. None hung without answers (`--answers none` directly):
each started within its normal time, without the features the answers
would have enabled; Codex drew about 0.5–1 s later.

Synchronized output passes through untouched (every TUI but Codex uses
it); no TUI here wrote OSC 52 or OSC 8 on its pre-login screens, so the
matrix has no evidence for them. OSC 52 and OSC 8 pass through a live
client as bytes; a redraw does not carry OSC 8 links (`vt100` keeps no
links).

## Fixes

Each has a `ROOT CAUSE` regression test; each test failed before its fix.

- **G11, `COLORTERM`.** Init cleared the environment and set `TERM` only.
  It now sets `COLORTERM=truecolor`, as herdr does for its panes.
  `the_driving_program_is_told_its_terminal_draws_24_bit_color`
  (`guest/init/program_test.rs`, Linux).
- **G12, the restore.** The relay's restore now also turns off SGR pixel
  and X10 mouse, color scheme reports, modifyOtherKeys (`CSI > 4 m`, reset
  to the terminal's initial value), sets autowrap and the default cursor
  shape and resets the cursor color (OSC 112).
  `the_restore_turns_off_what_tuis_turn_on_beyond_the_input_modes`
  (`relay/terminal_test.rs`), and `tui replay` of the `detach` captures.
- **G13, the redraw.** The model tracks modifyOtherKeys, color scheme
  reports, autowrap and the cursor color; the redraw's reset clears them
  and the redraw sets them again.
  `redraw_restores_the_modes_tuis_set_beyond_input_modes`
  (`process/screen_test.rs`).
- **G14, DA1.** The executor answers `CSI c` and `CSI 5 n` from the model
  when no client terminal that can type is attached, and a departing
  client's unanswered queries are answered as asked (before, always as a
  cursor position report).
  `primary_device_attributes_are_answered_and_the_client_terminals_features_are_not`,
  `a_detached_program_that_asks_for_its_terminals_attributes_gets_an_answer`,
  `an_attributes_query_a_departing_client_left_unanswered_is_answered`.

## Not fixed

- **G15, a TUI started with no client terminal.** needy passes the
  client's input on as bytes, so it cannot answer for a terminal that has
  not attached yet; herdr answers from its model because it encodes keys
  again for each pane. Answering the kitty flags or colors for an unknown
  client would make a TUI expect encodings the client may not send. `NOTICE`
  at the answers in `process/screen.rs`. `run create -it` waits for its
  client before the program starts, so only `run create -t` and
  `exec -d -t` are affected.
- **G16, ctrl-z.** The terminal's program leads its own session, and its
  parent is in another one, so its process group is orphaned and Linux
  discards the SIGTSTP it sends itself
  ([`kernel/signal.c#L2847-L2864`](https://github.com/torvalds/linux/blob/v6.12/kernel/signal.c#L2847-L2864));
  checked in a `macos-vz` actor: `kill -TSTP 0` returned and the shell
  stayed `S (sleeping)`. pi and opencode then wait for a SIGCONT nobody
  sends. tmux continues a stopped pane process at once
  ([`server.c#L460-L524`](https://github.com/tmux/tmux/blob/3.5a/server.c#L460-L524)),
  herdr has no job control at all; either way needy would need a
  job-control parent inside the terminal's session, a change of the
  process model. `TODO(job-control)` in `process/pty.rs`.
- **Smaller.** DECXCPR is still not answered (`TODO(terminal-queries)`);
  OSC 8 links are not redrawn; the window title is not restored after a
  detach (a local terminal does not restore it either).

## Platforms

| Platform | Run | Result |
| --- | --- | --- |
| Apple silicon Mac, `macos-vz` | whole matrix, before and after | as the table |
| Apple silicon Mac, `macos-apple-container` | whole matrix, after | as `macos-vz` after |
| Linux KVM host (x86_64), `linux-firecracker` | whole matrix, after (`exec` rerun after a harness fix that passes `IMAGE` to it) | as `macos-vz` after: `create` and `attach` equal for pi, Codex and Claude Code, opencode's random tip line, herdr's prompt and title; nothing left on after a detach |
| Linux KVM host, `linux-container` | started with a daemon that is not root | not verified: the host has no `sshfs`, which the workspace mount needs, and installing it needs root |

On `macos-apple-container`, a first run hit the copy-in ownership bug
fixed meanwhile on `main` (`f804539`: opencode's `EACCES ... mkdir
'/home/agent/.local/state'`); opencode and Claude Code ran again after the
rebase, as the table. A Claude Code run sometimes ended at "Unable to
connect to Anthropic services" before its first screen; those runs were
repeated.

`make check` and `make test` pass on the Mac and on the Linux KVM host.
The relay change is shared code; Windows is covered by CI's build, Clippy
and tests, since no Windows terminal path is in the matrix.
