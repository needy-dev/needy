# Installation and Prerequisites

Released binaries and base VM images are published on
[GitHub Releases](https://github.com/needy-dev/needy/releases) with `.sha256`
checksums and build provenance attestations
(`gh attestation verify <asset> -R needy-dev/needy`). Container images and agent
Dev Container features are published on GHCR (`ghcr.io/needy-dev/needy-actor`,
`ghcr.io/needy-dev/needy-executor`, and `ghcr.io/needy-dev/features/*`).

## Quick install

**Linux** (installs `needy` and starts the control daemon user service):

```sh
curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
```

**Apple silicon macOS** (installs `needy`, the guest needy, and `vfkit`, then starts the daemon):

```sh
brew install needy-dev/tap/needy vfkit
needy daemon service install --now
```

## Other install methods

- **Install script options**: `install.sh` verifies `.sha256`, installs to
  `~/.local/bin` (`NEEDY_INSTALL_DIR`, `NEEDY_VERSION`), and installs/replaces
  the user daemon service (`NEEDY_NO_DAEMON=1` skips the daemon on runner-only
  machines). Works on Linux and macOS.
- **proto**:
  ```sh
  proto plugin add needy "https://raw.githubusercontent.com/needy-dev/needy/main/toolchain/proto/needy.toml" --to global
  proto install needy latest --config-mode global --pin global
  ```
- **Windows (executor only)**: download
  [needy-x86_64-pc-windows-msvc.zip](https://github.com/needy-dev/needy/releases/latest/download/needy-x86_64-pc-windows-msvc.zip),
  extract `needy.exe`, and add it to `PATH`.
- **Manual / offline image pulls**: local `run create` and `image build` pull
  missing release base images automatically. For offline or pre-warmed hosts,
  use `needy image pull` (`--driver`, `--file ARCHIVE`), `needy image ls`,
  `needy image prune`, and `needy image rm`.

## Build from source

```sh
git clone https://github.com/needy-dev/needy && cd needy
cargo install --path crates/cli          # installs `needy`
make guest                               # bin/needy-linux-<arch>, with Docker
cp bin/needy-linux-* "$(dirname "$(command -v needy)")/"
```

Actor images hold no needy binary: the daemon injects its own static Linux
**guest needy** (`needy-linux-<arch>` next to `needy`, or `--guest-needy PATH`).
A dev build has no GitHub release to pull base images from; build them from the
checkout:

```sh
needy image build                                  # host VM base image (needs checkout)
needy image build --linux-container --executor     # or --macos-apple-container on Mac
needy config set daemon.executor_image needy-executor:latest
needy daemon service install --now
```

## Daemon service management & uninstall

`needy daemon service status` checks the user service
(`~/.config/systemd/user/needy.service` on Linux, launchd on macOS). System
services use `--scope system --run-as USER` (root required). After
`brew upgrade needy`, run `needy daemon service install --replace --now`. For
foreground serving during development, run `needy daemon serve`.

To uninstall (keeping state and data directories):

```sh
needy daemon service uninstall                            # if installed
brew uninstall needy                                      # Homebrew
rm -f ~/.local/bin/needy ~/.local/bin/needy-linux-*       # install script
proto uninstall needy && proto plugin remove needy --from global   # proto
cargo uninstall needy-cli                                 # from source
```

## Workspace mount prerequisites (`linux-container` actor)

For a `linux-container` actor, install SSHFS and FUSE 3 on the **Linux actor
host** (VM actors bundle their own helpers):

<!-- Package sources: docs/ai/references/cli/2026-11-15-workspace-mount-package-sources-note.md -->

```sh
# Debian / Ubuntu
sudo apt-get update && sudo apt-get install -y sshfs fuse3

# Fedora
sudo dnf install -y fuse-sshfs fuse3

# Arch Linux
sudo pacman -S --noconfirm sshfs fuse3
```

Enable `user_allow_other` for the non-root daemon, then verify as the daemon
account without `sudo`:

```sh
sudo sh -c 'grep -qxF user_allow_other /etc/fuse.conf || echo user_allow_other >> /etc/fuse.conf'
command -v sshfs && command -v fusermount3
test -r /dev/fuse && test -w /dev/fuse
docker info --format '{{.ServerVersion}}'
```
