# Workspace mount package sources

**Evidence level: documentation inspection.** This note records the package
names and helper files used by the [README](../../../../README.md#workspace-mount-prerequisites)
and the [setup guide](../../../../apps/docs/src/setup/guide.ts).
The package references do not establish fresh-install behavior.

The distribution indexes are mutable. Retrieval date: **2026-11-15**.
Their URLs do not identify immutable revisions. The retrieval date records
when these package names and file lists were examined.

| Distribution | Package data | Reference |
| --- | --- | --- |
| Ubuntu 24.04 | `sshfs` depends on `fuse3` | [SSHFS package](https://packages.ubuntu.com/noble/sshfs) |
| Debian 13 | `fuse3` provides `/usr/bin/fusermount3` | [FUSE 3 file list](https://packages.debian.org/trixie/amd64/fuse3/filelist) |
| Fedora 43 | `fuse-sshfs` depends on `fuse3` | [SSHFS package](https://packages.fedoraproject.org/pkgs/fuse-sshfs/fuse-sshfs/fedora-43.html) |
| Fedora 43 | `fuse3` provides `/usr/bin/fusermount3` | [FUSE 3 file list](https://packages.fedoraproject.org/pkgs/fuse3/fuse3/fedora-43.html#files) |
| Arch Linux | `sshfs` depends on `fuse3` | [SSHFS package](https://archlinux.org/packages/extra/x86_64/sshfs/) |
| Arch Linux | `fuse3` provides `/usr/bin/fusermount3` | [FUSE 3 file list](https://archlinux.org/packages/extra/x86_64/fuse3/files/) |
| Ubuntu 24.04 | `openssh-sftp-server` provides `/usr/lib/openssh/sftp-server` | [SFTP server file list](https://packages.ubuntu.com/noble/amd64/openssh-sftp-server/filelist) |

The tagged [libfuse 3.16.2 configuration](https://github.com/libfuse/libfuse/blob/fuse-3.16.2/util/fuse.conf#L3-L9)
is the pinned source for `user_allow_other`. Its comments explain how that
setting permits non-root users to use `allow_other`.
