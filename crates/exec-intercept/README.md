# needy-exec-intercept

Linux x86-64/aarch64 process-tree supervision with selective `execve` and
`execveat` interception. Depends only on `libc`; no Needy, networking, Docker or
async-runtime dependency. The package name is provisional and it has not
been published. A license must be selected before publication.

Call `supervise` on a dedicated OS thread with a `Command`, cancellation flag
and placement callback. Standard streams are inherited. The callback returns
`Continue`, `Deny(errno)` or `Redirect(Replacement)`. Arguments use `OsString`
and preserve non-UTF-8 bytes. Intercepted strings share a 2 MiB budget; each
argv/env vector is limited to 32,768 entries. Replacement storage is also bounded
to 2 MiB.

The root's first exec stays local. A redirected process and its descendants
remain internal, preventing recursive interception of proxy runtimes. Normal
execution uses `PTRACE_CONT`; redirect allocation, denial and vfork cleanup use
targeted syscall stops. Replacement storage comes from an injected anonymous `mmap`,
not an assumed free stack region. The mapping disappears on successful exec.
For `vfork`, targeted parent-side `munmap` removes the mapping from the shared
address space before the parent returns to userspace.

Only the native 64-bit ABI is supported; 32-bit compatibility executables are
not supported. The supervisor sets no-new-privileges in its initial child.
The kernel must permit ptrace and seccomp trace events. The library owns only
children created by the calling OS thread, and kills/reaps its tracees on an
error or cancellation. It is not a sandbox and does not virtualize files,
descriptors, networks, remote grandchildren or executable lookup before exec.
The caller is responsible for terminal foreground groups and process routing.
The process namespace must have a working init/reaper for adopted orphans; use
Docker's `--init` when running the kernel tests in a container.

Run the Linux tests against an actual kernel; compiling on macOS only checks
the public types and returns an explicit unsupported-platform error at runtime.

Try the standalone example on Linux:

```sh
cargo run -p needy-exec-intercept --example redirect
cargo test -p needy-exec-intercept --test kernel -- --test-threads=1
cargo package -p needy-exec-intercept --allow-dirty
```

The example keeps `/bin/sh` local and replaces its `/usr/bin/printf` exec with
`/bin/echo`, printing `redirected`. For a container, use `--init` and permit
ptrace/seccomp explicitly, for example `--security-opt seccomp=unconfined` in
an isolated test container. The implementation requires no root capability to
trace its own child on the tested Linux configurations.

The crate stores byte-preserving argument and environment values. The Needy
network bridge currently accepts UTF-8 command arguments and explicitly approved
environment variables; that product restriction does not belong to this crate.
