# Contributing to Needy

Thank you for contributing to Needy. For architecture details, crate boundaries,
coding conventions, and testing rules, see [`AGENTS.md`](AGENTS.md) and
[`docs/guides/development.md`](docs/guides/development.md).

## Development setup

```sh
make build
make check
make test               # on macOS, run `ulimit -n 4096` first
make proto              # regenerate crates/api-proto/src/gen after editing proto/
cargo xtask e2e         # run the end-to-end matrix supported by this host
```

On macOS, raise the open-file limit (`ulimit -n 4096`) before running `make test`
or `cargo test -p needy-daemon`; the default 256-fd soft limit is too low for the
parallel daemon test suite.

## Pull requests and commits

- Use Conventional Commits: `<type>(<scope>): <message>` (for example,
  `fix(daemon): ...` or `docs: ...`).
- Keep changes focused on a single slice. Add focused unit or regression tests
  for behavior changes, and run `make check` and `make test` before opening a PR.
- Do not edit `crates/api-proto/src/gen` by hand; edit `proto/` and run
  `make proto`.
- Never commit private data (real names, emails, SSH host aliases, local paths,
  IP addresses, hostnames, or tokens). Use reserved placeholders such as
  `example.com`, `192.0.2.0/24`, `/home/user`, and `/Users/user`.

## Releasing

Every release **must** go through a **pre-release** first and only be promoted to
a stable release after the `Release` workflow has finished uploading all assets.

Do **not** publish a tag directly as a stable release (`gh release create vX.Y.Z`
without `--prerelease`):

- `.github/workflows/release.yml` triggers on `release: [published]` (for both
  pre-releases and stable releases) and takes ~15–20 minutes to build, sign,
  notarize, attest, and upload binaries, checksums, and container images.
- `.github/workflows/update-homebrew.yml` triggers on `release: [released]`,
  which fires immediately when a stable release is created directly — racing
  with `Release` and failing because the macOS `.sha256` assets do not exist yet.
- GitHub's `releases/latest` (used by `install/install.sh` and Windows download
  links) skips pre-releases. Publishing directly as stable points `latest` at an
  empty release until `Release` finishes.

### Release steps

1. Bump the workspace version in `Cargo.toml` / `Cargo.lock` in a PR
   (`chore(release): prepare vX.Y.Z`) and merge it into `main`.
2. Create and push the annotated or lightweight tag `vX.Y.Z` on `main`, then
   publish it as a **pre-release**:

   ```sh
   gh release create vX.Y.Z \
     --prerelease \
     --title "vX.Y.Z" \
     --generate-notes
   ```

   This triggers `Release` (`release: [published]`) without triggering
   `Update Homebrew` or moving `releases/latest`.
3. Wait for the `Release` workflow to succeed and verify that the release assets
   (including `needy-aarch64-apple-darwin.tar.gz.sha256` and
   `needy-x86_64-apple-darwin.tar.gz.sha256`) and GHCR images are published.
4. Promote the pre-release to a stable release:

   ```sh
   gh release edit vX.Y.Z --prerelease=false
   ```

   Promoting fires `release: [released]`, which atomically updates
   `releases/latest` and triggers `Update Homebrew` against the already-uploaded
   checksums.

If a release was accidentally published directly as stable and `Update Homebrew`
failed while `Release` was still running, wait for `Release` to complete and
re-run `Update Homebrew` (or dispatch it manually):

```sh
gh workflow run update-homebrew.yml -f tag_name=vX.Y.Z
```
