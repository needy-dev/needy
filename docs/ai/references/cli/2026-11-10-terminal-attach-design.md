# Terminals, attach and the interactive actor

**Status**: design, 2026-11-10. Nothing here is implemented. The gap list is
verified against the code at `e4c6480`; the design follows a study of
[herdr](https://github.com/ogulcancelik/herdr) (Rust, a server that owns PTY
panes and redraws them from a terminal model) and of an earlier Go project by
the owner that bridges PTYs to remote clients. Items marked *fix* are bug fixes;
the rest needs owner approval per item.

Related notes:

- [CLI, exec streams and reports](2026-11-09-cli-exec-and-report-design.md)
- [Runners and runs](../architecture/2026-11-10-runner-and-run-design.md):
  `needy run -it` and `needy run attach` use what this note describes.

## Gaps today

| ID | Gap | Where | Kind |
| --- | --- | --- | --- |
| G1 | Attaching to a full-screen TUI shows garbage: the replay is the last 256 KiB of raw bytes, which can start inside the alternate screen or an escape sequence and turns the program's modes on in the attaching terminal | `core-executor` `process/proc.rs` replay | design |
| G2 | An attach carries no window size; the client sends one only on SIGWINCH, so an attacher of the same size gets no redraw | `ExecAttach`, `cli` `attach.rs`, `relay/signals.rs` | *fix* |
| G3 | No detach key: leaving means killing the client; leaving a non-detached `exec -t` sends SIGHUP to the process | `relay`, `service.rs` | design |
| G4 | Only raw mode is restored on exit; alternate screen, mouse, bracketed paste, focus and keyboard modes leak into the user's shell | `relay/terminal.rs` | *fix* |
| G5 | One slow client stalls the program and every other client: output fan-out awaits each subscriber in turn under a lock | `process/proc.rs` emit | *fix* |
| G6 | No arbitration: input from all clients interleaves, any client's resize wins, no read-only viewer | `process/` | design |
| G7 | End of input on a PTY is written as `0x04`, which a raw-mode TUI reads as Ctrl-D | `process/pty.rs` | *fix* |
| G8 | Possible exit hang: if a descendant keeps the PTY open, the reader never sees end of file and `Exited` is never sent (repro to write: `exec -t sh -c 'setsid sleep 600 & exit 3'`) | `process/pty.rs` | *fix* if the repro fails |
| G9 | The client's `TERM` is forwarded even when the executor lacks the terminfo entry; `COLORTERM` is not forwarded | `relay`, exec start | *fix* |
| G10 | The actor's driving program runs with stdin `/dev/null` and output to the serial console: it cannot be interactive | `core-actor` `guest/init/driver.rs` | design |

Smaller: window sizes are cast to `u16` without clamping; the PTY input queue is
unbounded, so a large paste grows the executor's memory.

## Design

### Terminal model for redraw (G1)

Each TTY process keeps, next to the raw replay buffer and under the same lock, a
terminal-state model fed with the same bytes (`vt100` crate; `alacritty_terminal`
or `termwiz` if `vt100` falls short). A new attach in terminal mode gets: a
terminal reset, the current screen drawn from the model, the program's input
modes (mouse, bracketed paste, focus, keyboard protocol), then live bytes. Pipes
and logs keep the raw buffer. `vt100` does not track the keyboard-protocol stack,
the title or the cursor shape; a small scanner records those.

### Attach behavior (G2–G7, G9)

- The attach request carries the window size.
- Terminal restore is idempotent and runs on exit, on a panic hook and on
  termination signals, and turns off everything the program may have turned on.
- `--detach-keys`, default `ctrl-]`, recognized also in the encoded forms
  terminals send with enhanced keyboard modes, never inside a paste. A `detach`
  request lets the client that started a process leave without ending it.
- Size ownership: the client that last sent input owns the size; when it leaves,
  the next most recent one takes over. `--read-only` attaches a viewer.
- Fan-out never blocks for terminal viewers: a viewer that falls behind skips
  output and later gets a fresh redraw from the model. Pipes stay lossless.
- End of input sends `0x04` only when the PTY is in canonical mode.
- `TERM` falls back to `xterm-256color` when the executor has no terminfo entry
  for the client's value, recorded in the trace; `COLORTERM` is forwarded.

### Interactive actor (G10)

Guest init runs the driving program on a PTY through the same process host as
executors (`core-executor`), with the user's uid and gid and a controlling
terminal, and serves `ProcessService` for it on a guest channel. Attaching to a
run's actor is then an ordinary attach with redraw, resize, signals and detach.
The control plane keeps one permanent reader so `run logs` and the trace keep
working without a client. The driving program keeps `TERM=xterm-256color`; it
talks to the model, not to each client's terminal.

### Trace events (optional)

needy has no agent concepts, so herdr-style "working / blocked / idle" states
do not fit. Generic `terminal.*` events might: output active and quiet edges,
title changes, bell and notifications, foreground process changes, the final
screen at exit.

## Windows

Neither studied project solves ConPTY for a process started from a service in
session 0, which is where needy's Windows executor stands. One of them ships a
pinned, newer ConPTY because the system one had bugs; the other closes the
pseudo console before draining output and creates the child suspended inside a
Job Object. Recorded for the Windows terminal work; not planned here.

## Order

1. Fixes: G2, G4, G5, G7, G9, and G8 after its repro test.
2. With approval: the terminal model and redraw (G1), detach keys (G3),
   arbitration and read-only (G6).
3. With the runners and runs design: the interactive actor (G10).
