// The shape of a profile, for the profile studio: which keys exist, which values an enum takes, and what each key
// does. needy rejects unknown keys (`#[serde(deny_unknown_fields)]`), so the studio does too.
//
// Source: https://github.com/needy-dev/needy/tree/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile
// NOTICE: this is a port of the serde structs there, cut to what the studio checks and offers. When a field or an
// enum value changes in crates/config/src/profile, change it here too.
// TODO(docs-studio): value types (durations, sizes, globs, `cpus`, `gpus`, `ulimits`, `mounts`) are not checked, only
// that the key exists; the daemon's `needy config profile check` is the real check. Port them if the studio needs them.

export type Field
  = | { kind: 'map', fields: Record<string, Field>, doc?: string, required?: string[] }
    /** A map whose keys the user names, such as `executors`; every value has the same shape. */
    | { kind: 'named', of: Field, doc?: string, extra?: Record<string, Field> }
    | { kind: 'list', of: Field, doc?: string }
    | { kind: 'enum', values: readonly string[], doc?: string }
    | { kind: 'scalar', doc?: string, suggest?: readonly string[] }
    /** Any value: a field whose inner shape the studio does not check. */
    | { kind: 'any', doc?: string }

const scalar = (doc?: string, suggest?: readonly string[]): Field => ({ kind: 'scalar', doc, suggest })
const any = (doc?: string): Field => ({ kind: 'any', doc })
const enumOf = (values: readonly string[], doc?: string): Field => ({ kind: 'enum', values, doc })

export const ACTOR_DRIVERS = ['linux-firecracker', 'linux-container', 'macos-vz', 'macos-apple-container'] as const
export const EXECUTOR_DRIVERS = ['native', 'linux-container'] as const
export const FILE_MODES = ['copy-in', 'mount', 'mirror-out'] as const
/** The domains a fault names exactly one of (profile/chaos.rs, `FaultDomain`). */
export const FAULT_DOMAINS = ['exec', 'file', 'net', 'sensor'] as const
/**
 * The errnos a file fault can deliver: sshfs turns every other SFTP status into EIO.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/chaos.rs#L172-L178
 */
export const FILE_ERRNOS = ['ENOENT', 'EACCES', 'EPERM', 'EOPNOTSUPP', 'EIO'] as const
/**
 * The signals an exec fault may end a command with: the standard signals whose default action ends the process.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/chaos.rs#L180-L213
 */
export const SIGNALS = ['SIGHUP', 'SIGINT', 'SIGQUIT', 'SIGILL', 'SIGTRAP', 'SIGABRT', 'SIGBUS', 'SIGFPE', 'SIGKILL', 'SIGUSR1', 'SIGSEGV', 'SIGUSR2', 'SIGPIPE', 'SIGALRM', 'SIGTERM', 'SIGSTKFLT', 'SIGXCPU', 'SIGXFSZ', 'SIGVTALRM', 'SIGPROF', 'SIGIO', 'SIGPWR', 'SIGSYS'] as const

const bool = (doc?: string): Field => scalar(doc, ['true', 'false'])

/** profile/network.rs: `ActorNetwork`, the actor's network at its gateway. */
// NOTICE: needy removed `inject` (header injection) and `upstream` on 2026-11-12; `services` replaces `upstream`.
const actorNetwork: Field = {
  kind: 'map',
  doc: 'What the gateway lets through.',
  fields: {
    allow: { kind: 'list', of: scalar(), doc: 'Host patterns that may be reached: `*`, `*.example.com` (also `example.com`), or a host. HTTPS is tunnelled as is.' },
    services: { kind: 'named', of: { kind: 'map', required: ['url'], fields: { url: scalar('The base URL requests go to, `http://` or `https://`, resolved on the control host.') } }, doc: 'Named plain-HTTP services: the actor asks for `http://NAME.needy/...` and the gateway forwards it to `url`.' },
  },
}

/** profile/network.rs: `NetworkPolicy`, the executor network. Nothing enforces it yet. */
const executorNetwork: Field = {
  kind: 'map',
  doc: 'The executor network; executors have no gateway yet, so nothing enforces it.',
  fields: {
    allow: { kind: 'list', of: scalar(), doc: 'Host patterns that may be reached.' },
    record: bool('Record request bodies in the trace.'),
  },
}

/** profile/runner.rs: a runner's name, or `{ labels: [...] }`. */
const runner = any('Where: a runner\'s name, or `{ labels: [KEY, KEY=VALUE] }`. The control host when absent.')

/**
 * profile/chaos.rs: one fault names exactly one domain (`exec`, `file`, `net` or `sensor`) and only that domain's
 * effects and triggers; check.ts checks which keys go together.
 */
const fault: Field = {
  kind: 'map',
  fields: {
    exec: scalar('A command pattern, written like a rule\'s `match`: `curl **`.'),
    file: { kind: 'map', required: ['open'], fields: { open: scalar('A glob on the path the actor opens: `**/*.json`, `~/workspace/src/**`.') }, doc: 'A file the actor opens on a workspace mount.' },
    net: { kind: 'map', fields: { connect: scalar('Every connection to a matching host.'), request: scalar('Plain-HTTP requests to a matching host only, such as a service\'s.') }, doc: 'A connection or plain-HTTP request at the gateway: `connect` or `request`.' },
    sensor: { kind: 'map', fields: { temp_c: scalar('Package temperature in degrees Celsius.') }, doc: 'The readings a sensor fault sets.' },
    exit: scalar('exec: the command exits with this code without running.'),
    signal: scalar('exec: the command dies of this signal without running.', SIGNALS),
    stderr: scalar('exec: written to stderr before the `exit`, `signal` or `hang`.'),
    errno: enumOf(FILE_ERRNOS, 'file: the open fails with this errno.'),
    refuse: bool('net: the connection is refused.'),
    reset: bool('net: the connection is closed without an answer.'),
    status: scalar('net: a plain-HTTP request (`net: { request: ... }`) is answered with this status.'),
    hang: bool('exec and net: the operation never completes, until its caller gives up or the run ends.'),
    delay: scalar('exec, file and net: hold the operation this long first, such as `2s`.'),
    probability: scalar('Fire on this share of matches, from the seed.'),
    nth: scalar('Fire on the nth match only, counted from 1.'),
    times: scalar('Fire at most this many times.'),
    after: scalar('Match only this long after the run was created, or later.'),
    until: scalar('Match only until this long after the run was created.'),
    at: scalar('sensor: when the readings are set, after the actor starts, such as `30s`.'),
  },
}

/** profile/rules.rs: `match`, `when` and the actions sit in one mapping. */
const actions: Record<string, Field> = {
  to: scalar('Where the command runs: an executor\'s name, `actor`, or `executor` for the default.'),
  executor: scalar('The executor for `to: executor`.'),
  deny: scalar('Deny the command with this message; it exits 126.'),
  timeout: scalar('Kill the command after this long, such as `10m`.'),
  cwd: scalar('Run in this directory.'),
  args: { kind: 'map', fields: { prepend: any(), append: any() } },
  wrap: any('Run the command inside these words.'),
  exec: any('Run these words instead.'),
  env: { kind: 'map', fields: { set: any(), default: any(), prepend: any(), append: any(), unset: any(), clear: scalar(), pass: any() } },
  files: { kind: 'list', of: scalar(), doc: 'File mappings for the commands this rule matches.' },
  use: { kind: 'list', of: scalar(), doc: 'Presets whose actions apply.' },
  final: scalar('Stop at this rule.', ['true', 'false']),
}

const rule: Field = {
  kind: 'map',
  required: ['match'],
  fields: {
    match: scalar('Written like the command it matches: `git **`, `"{node,bun} {script} **"`.'),
    when: { kind: 'map', fields: { has: any(), cwd: scalar(), env: any(), under: scalar(), tty: scalar(undefined, ['true', 'false']), captures: any() }, doc: 'Conditions on the command\'s context.' },
    ...actions,
  },
}

/** profile/executor.rs */
const executor: Field = {
  kind: 'map',
  fields: {
    runner,
    driver: enumOf(EXECUTOR_DRIVERS, '`native`: the runner\'s own process space. `linux-container`: a container through a Docker-compatible engine.'),
    container: any('Typed engine options for a linux-container executor: image, build, cpus, memory, env, mounts, gpus, …'),
    recipe: scalar(),
    workspace: scalar('The executor\'s workspace directory.'),
    path_map: any('Actor paths to this executor\'s paths.'),
    key: scalar('Which commands share an instance: `{run}`, `{group}`, `{profile}` or `{rule}`.', ['"{run}"', '"{group}"', '"{profile}"', '"{rule}"']),
    max_procs: scalar(),
    on_full: enumOf(['wait', 'reject']),
    max_instances: scalar(),
    idle_timeout: scalar(),
  },
}

/** profile/files.rs: every key other than `ignore_file` is a mapping. */
const mapping: Field = {
  kind: 'map',
  required: ['mode', 'from', 'to'],
  fields: {
    mode: enumOf(FILE_MODES, '`copy-in` when the run starts, `mirror-out` when it ends (or continuously), `mount` live.'),
    from: scalar('`host:PATH`, `actor:PATH` or `executor:NAME:PATH`.'),
    to: scalar('`host:PATH`, `actor:PATH` or `executor:NAME:PATH`.'),
    filter: { kind: 'list', of: scalar(), doc: 'rclone-style rules: `- /cache/**`, `+ **`.' },
    writable: scalar(undefined, ['true', 'false']),
    when: enumOf(['continuous', 'end']),
    delete: scalar(undefined, ['true', 'false']),
  },
}

export const PROFILE: Field = {
  kind: 'map',
  required: ['actor'],
  fields: {
    extends: scalar('Another profile this one builds on.'),
    user: { kind: 'map', fields: { name: scalar(), uid: scalar(), gid: scalar() }, doc: 'The one user of the actor and every executor.' },
    actor: {
      kind: 'map',
      required: ['driver'],
      doc: 'Where the driving program runs.',
      fields: {
        driver: enumOf(ACTOR_DRIVERS, 'Which isolation runs the actor.'),
        image: scalar('The actor image, an OCI reference: the container drivers run it, the VM drivers boot `needy image build --from` of it.'),
        runner,
        recipe: scalar('The machine the actor presents.', ['builtin:gce-ubuntu-22.04']),
        limits: { kind: 'map', fields: { single_process: scalar(), self_replace: scalar() } },
        network: actorNetwork,
        command: any('The driving program and its arguments.'),
        env: any(),
        workspace: {
          kind: 'map',
          required: ['path'],
          doc: 'The directory the work operates on, and the program\'s working directory; the home directory when absent.',
          fields: {
            path: scalar('The directory in the actor; `~` is the user\'s home.'),
            from: scalar('`executor:NAME:PATH` (a live mount), `host:PATH` (copied in), or absent for a fresh directory.'),
            writable: bool('The actor may change an executor\'s files; with `host:`, mirrored back when the run ends.'),
          },
        },
        container: { kind: 'map', fields: { oci_runtime: enumOf(['runc', 'runsc'], '`runsc` is gVisor.') }, doc: 'For linux-container and macos-apple-container actors.' },
      },
    },
    executors: { kind: 'named', of: executor, doc: 'Where routed work takes effect.' },
    executor_network: executorNetwork,
    presets: { kind: 'named', of: { kind: 'map', fields: actions }, doc: 'Named actions that rules `use`.' },
    commands: { kind: 'list', of: rule, doc: 'Where each exec runs. Every matching rule applies in order.' },
    files: { kind: 'named', of: mapping, extra: { ignore_file: scalar() }, doc: 'Named file mappings between host, actor and executors.' },
    run_files: { kind: 'list', of: scalar(), doc: 'The file mappings every run uses.' },
    chaos: { kind: 'map', fields: { seed: scalar('Mixed with each fault\'s name and match count, so a replay fires the same faults.'), faults: { kind: 'named', of: fault, doc: 'Faults by name, checked in name order; the first that fires on an operation is the one it gets.' } }, doc: 'Named faults that fire from a seed.' },
    resolvers: any('Launchers to see through, such as `npx` or `uv run`.'),
    unknown: scalar('Where commands of unknown shell scripts go: `sticky`, `actor` or an executor.'),
    default_executor: scalar(),
    trace: { kind: 'map', fields: { sinks: { kind: 'list', of: enumOf(['sqlite', 'jsonl']) }, timing: scalar(undefined, ['true', 'false']) }, doc: 'Where events go.' },
  },
}
