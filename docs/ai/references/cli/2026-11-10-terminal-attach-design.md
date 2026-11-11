# Terminals, attach and the interactive actor

**Status**: implemented, updated 2026-11-12. G1–G10 are implemented;
automated and live-probe evidence for the interactive actor is in
[Interactive actor: implementation and evidence](2026-11-11-interactive-actor-evidence.md).
The original gap list was verified against the code at `e4c6480`; the
design follows a study of [herdr](https://github.com/ogulcancelik/herdr) (Rust, a server that owns PTY
panes and redraws them from a terminal model) and of an earlier Go project by
the owner that bridges PTYs to remote clients. Items marked *fix* are bug fixes;
the remaining design items received owner approval in the runners-and-runs work.

Related notes:

- [CLI, exec streams and reports](2026-11-09-cli-exec-and-report-design.md)
- [Runners and runs](../architecture/2026-11-10-runner-and-run-design.md):
  `needy run -it` and `needy run attach` use what this note describes.

## Gaps today

| ID | Gap | Where | Kind | Status |
| --- | --- | --- | --- | --- |
| G1 | Attaching to a full-screen TUI shows garbage: the replay is the last 256 KiB of raw bytes, which can start inside the alternate screen or an escape sequence and turns the program's modes on in the attaching terminal | `core-executor` `process/proc.rs` replay | design | implemented: draw the current screen from `vt100`, then restore tracked input modes |
| G2 | An attach carries no window size; the client sends one only on SIGWINCH, so an attacher of the same size gets no redraw | `ExecAttach`, `cli` `attach.rs`, `relay/signals.rs` | *fix* | fixed: `ExecAttach.window`; the executor resizes the terminal after subscribing the client |
| G3 | No detach key: leaving means killing the client; leaving a non-detached `exec -t` sends SIGHUP to the process | `relay`, `service.rs` | design | implemented: `--detach-keys`, default `ctrl-]`; enhanced keyboard encodings recognized, pasted content excluded |
| G4 | Only raw mode is restored on exit; alternate screen, mouse, bracketed paste, focus and keyboard modes leak into the user's shell | `relay/terminal.rs` | *fix* | fixed: restore on the relay's end, a panic hook and a second SIGTERM or SIGHUP; the alternate screen is left only when the output entered it |
| G5 | One slow client stalls the program and every other client: output fan-out awaits each subscriber in turn under a lock | `process/proc.rs` emit | *fix* | fixed: the starting client keeps its backpressure; terminal viewers skip output and receive a fresh redraw as soon as they can take it, without waiting for new output; lagging pipe viewers are disconnected |
| G6 | No arbitration: input from all clients interleaves, any client's resize wins, no read-only viewer | `process/` | design | implemented: size belongs to the last client that attached or typed; read-only viewers send no input, signals or size |
| G7 | End of input on a PTY is written as `0x04`, which a raw-mode TUI reads as Ctrl-D | `process/pty.rs` | *fix* | fixed: the terminal's VEOF only with ICANON; in raw mode input just stops |
| G8 | Possible exit hang: if a descendant keeps the PTY open, the reader never sees end of file and `Exited` is never sent (repro to write: `exec -t sh -c 'setsid sleep 600 & exit 3'`) | `process/pty.rs` | *fix* if the repro fails | fixed: reproduced on Linux, not on macOS, once the descendant has its own session before the exit (the literal repro usually loses that race and its descendant dies of the hangup); the output reader stops after the 500 ms drain |
| G9 | The client's `TERM` is forwarded even when the executor lacks the terminfo entry; `COLORTERM` is not forwarded | `relay`, exec start | *fix* | fixed: `xterm-256color` when the executor has no terminfo entry, recorded as `term_fallback` in `exec.start`; `COLORTERM` forwarded |
| G10 | The actor's driving program runs with stdin `/dev/null` and output to the serial console: it cannot be interactive | `core-actor` `guest/init/driver.rs` | design | implemented: `run create -t/-it`, `run attach`; guest PTY and process service over vsock, independent console reader |

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
  See [A lagging terminal viewer](#a-lagging-terminal-viewer).
- End of input sends `0x04` only when the PTY is in canonical mode.
- `TERM` falls back to `xterm-256color` when the executor has no terminfo entry
  for the client's value, recorded in the trace; `COLORTERM` is forwarded.
- A cursor position query (`CSI 6 n`) is answered by the client terminal that
  shows the program when one that can type is attached, and otherwise by the
  executor from the model (a detached process, or one only read-only viewers
  watch; also a query a departing client left unanswered). ConPTY asks once
  at start and reads no input until answered, so `exec -d -t` on Windows
  depends on this. Two writable clients still both answer.

### A lagging terminal viewer

The starting client is the one subscriber the program waits for. Every
other client, a viewer, has a queue of 64 output events (`SUBSCRIBER_QUEUE`
in `process/proc.rs`) and never holds back the program, the starting client
or another viewer. On pipes a viewer whose queue is full is disconnected and
told it fell behind, so what it does get is never missing a part.

On a terminal a viewer whose queue is full skips output instead and has one
redraw pending: a single slot, however much it skipped. The redraw is the
current screen drawn from the model (as for a new attach), and it is
delivered as soon as the viewer can take it, whichever comes first:

- the viewer reads everything queued: its next read takes the redraw, even
  when the program writes nothing more (a shell back at its prompt after a
  burst);
- the program writes again while the viewer's queue has room: the redraw is
  queued before that output, which it already shows.

The redraw is taken under the fan-out's lock, so nothing is queued between
it and the output that follows. needy keeps no copy of what each viewer
shows and never compares screens, so a viewer that skipped output is always
drawn again, also when the screen's text did not change (the skipped output
may have changed a mode or the cursor). If the program exits while a
viewer's queue has no room for the redraw and the exit, that viewer is
disconnected and told it fell behind.

This follows [herdr](https://github.com/ogulcancelik/herdr/tree/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde),
read at `ee5bfb50`:

- Each client has a render lane of one frame, "Capacity is one so slow
  clients cannot build lag"
  ([`client_transport.rs#L121-L127`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/client_transport.rs#L121-L127)).
- A frame that a full lane cannot take is skipped, the client's baseline is
  not committed and the client is marked for a full render, for that client
  only
  ([`headless/render.rs#L963-L991`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless/render.rs#L963-L991)).
- When the client's writer takes a frame from the lane it reports
  `ClientWriterDrained`
  ([`client_transport.rs#L951-L971`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/client_transport.rs#L951-L971)),
  and the server renders the pending full frame
  ([`headless.rs#L2651-L2656`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless.rs#L2651-L2656));
  "The writer-drained event schedules recovery, even if pane output stops"
  ([`headless/render.rs#L561-L569`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless/render.rs#L561-L569)).
- Frames render on their own cadence, at most every 16 ms
  ([`app/mod.rs#L39`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/app/mod.rs#L39)),
  not only on new output.
- Clients were left stale where a frame compared equal to the last one
  (commits [`61f64006`](https://github.com/ogulcancelik/herdr/commit/61f64006)
  and [`c9fca9fb`](https://github.com/ogulcancelik/herdr/commit/c9fca9fb)); the server now forces a fresh frame
  ([`headless.rs#L768-L773`](https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless.rs#L768-L773)).

needy streams the program's bytes rather than frames, so it adapts this: the
queue keeps the output a viewer has room for, and only a viewer that skipped
some gets a frame, its redraw. It needs no render cadence: the viewer's own
read asks for the pending redraw, as herdr's writer does. The regression
tests are in `process/mod_test.rs`:
`a_lagging_viewer_is_redrawn_when_it_catches_up_although_the_program_stays_quiet`
and `a_lagging_viewer_is_redrawn_although_the_skipped_output_left_the_text_unchanged`.

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
