# File filters and state between runs

A file mapping's `filter` selects which files needy copies in or mirrors out.
The outer list is YAML. Each string contains a rclone-style include or exclude rule.
Needy implements these rules itself. It does not run rclone or use regular expressions.

## How the rules work

```yaml
filter: ["+ /auth.json", "- **"]
```

Needy reads the rules in order. The first matching rule decides whether to include a path.
Paths with no matching rule are included. An empty filter includes everything.

| Syntax | Meaning | Example |
| --- | --- | --- |
| `+ PATTERN` | Include matching paths | `+ /auth.json` |
| `- PATTERN` | Exclude matching paths | `- *.log` |
| Leading `/` | Match from the mapping root | `/auth.json` matches the root file, not `nested/auth.json` |
| No leading `/` | Match at any directory level | `*.log` also matches `nested/debug.log` |
| `**` | Match across directory levels | `/sessions/**` matches the root sessions directory and its contents |
| Trailing `/` | Match a directory and its contents | `/cache/` excludes the root cache directory with `- /cache/` |

The mapping root is the directory that the mapping transfers.
For a mapping of `~/.codex/`, `/auth.json` means `~/.codex/auth.json`, not the system path `/auth.json`.

The two rules in the first example include only the root `auth.json` file.
The final `- **` excludes everything else. Reversing the rules excludes everything, including `auth.json`.

## Keep only an authentication file between runs

This profile fragment imports `auth.json` at run start and saves it at run end.
Both mappings use the same filter, so other files stay outside the saved state.

```yaml
files:
  auth-in:
    mode: copy-in
    from: "host:~/needy-state/codex/"
    to: "actor:~/.codex/"
    filter: ["+ /auth.json", "- **"]
  auth-out:
    mode: mirror-out
    from: "actor:~/.codex/"
    to: "host:~/needy-state/codex/"
    filter: ["+ /auth.json", "- **"]
```

Create the host directory before the first run:

```sh
mkdir -p ~/needy-state/codex
```

If the profile has a `run_files` list, add `auth-in` and `auth-out` to it.
Without that list, the run selects all file mappings.

The `host:` directory is on the control daemon's machine.
After a run saves the file, the next run imports it from the same directory.
The actor can read the imported credential.

Normal exit and graceful `needy run stop` allow the actor to mirror out.
`needy run stop --now` skips the actor's mirror-out. A control-plane crash can also prevent the save.
Filtering does not remove existing destination files or propagate deletions by default.

## Keep authentication and configuration

To keep both root files, use this filter on the import and export mappings:

```yaml
filter:
  - "+ /auth.json"
  - "+ /config.toml"
  - "- **"
```

This excludes sessions, logs and other files.
The block list and the inline list (`["+ /auth.json", "- **"]`) are equivalent YAML forms.

## Exclude caches and logs

This filter keeps other files and excludes the root cache directory and log files at any depth:

```yaml
filter:
  - "- /cache/"
  - "- *.log"
  - "+ **"
```

For example, it keeps `settings.json` and `projects/session.jsonl`.
It excludes `cache/data`, `debug.log` and `projects/debug.log`.
The final `+ **` makes the include-by-default behavior explicit.

## Keep selected files and a directory

This filter keeps two root files and the root `projects` directory:

```yaml
filter:
  - "+ /settings.json"
  - "+ /.claude.json"
  - "+ /projects/**"
  - "- **"
```

Needy skips excluded directories and their contents.
For nested files, include their parent directories too, or map the selected subdirectory separately.
For example, `+ /projects/**` permits traversal into `projects/` before the final exclusion.
These paths select saved files. They do not establish which files an application's login requires.

The [Codex profile](codex.yaml) and
[Claude Code profile](claude.yaml) show complete profiles that save their configuration directories.

## Evidence

Evidence level: implementation and automated unit tests.
The [filter rules](../../crates/common/src/filter.rs) cover ordered matching, root anchoring and nested paths.
The [archive tests](../../crates/common/src/archive.rs) cover transfer filters and excluded parent directories.
The [daemon tests](../../crates/daemon/src/files_test.rs) cover filtered mirror-out from actors and executors.
These checks establish file selection, not authenticated access or token refresh across runs.
