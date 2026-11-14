# Cloudflare docs deployment

Approved feature: deploy the existing `apps/docs` site and its PR previews.
The application layout and build pipeline stay in `apps/docs`.

## Sources

The three workflow files retain airi's production/prepare/deploy split:

- [airi workflows](https://github.com/moeru-ai/airi/tree/936008404e1e48461f531ebf51b407ed8b31576d/.github/workflows)
- [injeca workflows](https://github.com/moeru-ai/injeca/tree/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/.github/workflows)
- [ergosphere-it workflows](https://github.com/moeru-ai/ergosphere-it/tree/96c945b4fc73e4b8546d5b9b941209c3fcce4dc9/.github/workflows)

The copied files carry their exact source links. Needy needs no external docs
checkout, shared-package build, storybook build, S3 secrets, or Turbo cache.
Each build installs the existing app lockfile and runs `pnpm --dir apps/docs build`.
The setup action pins pnpm and reads the lockfile from `apps/docs`.

## Production

`deploy-cloudflare-workers.yml` deploys pushes to `main` and manual runs.
The `Production` GitHub environment links to <https://needy.ayaka.io/>.
The Worker name is `needy`; its workers.dev address is
<https://needy.ayakamoe.workers.dev/> in the configured Cloudflare account.

`apps/docs/wrangler.toml` declares the custom domain and enables workers.dev
and version URLs. Static assets come from `dist/`. The SPA fallback serves
`index.html` for `/studio` and other paths without an asset.
See [static assets](https://developers.cloudflare.com/workers/static-assets/get-started/)
and [custom domains](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/).

Both deploy workflows use the repository secrets `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID`. The token needs access to deploy Workers and to bind
the custom domain in its Cloudflare zone.

## Pull request previews

1. `deploy-cloudflare-workers-preview-prepare.yml` runs without Cloudflare
   secrets. It stores the PR number, source repository, and exact head SHA in
   the `preview-meta` artifact. The trigger requires a PR event.
2. `deploy-cloudflare-workers-preview-deploy.yml` handles the successful
   prepare run. Its first job posts the approval link.
3. The `Preview` GitHub environment requires a maintainer review, as in airi.
   That gate covers the job that checks out and builds PR code, including forks.
   Metadata passes the same shape checks as airi before checkout.
4. The deploy job builds the exact recorded head, then uploads a Worker version
   with `--preview-alias pr-N`. It does not deploy that version to production.
5. The workflow updates the PR comment with the version URL and the stable
   `https://pr-N-needy.ayakamoe.workers.dev` alias.

Preview URLs follow [Cloudflare's version URL model](https://developers.cloudflare.com/workers/versions-and-deployments/version-urls/).
The `Preview` environment rule is repository configuration; removing it removes
this approval gate. A workflow comment alone does not enforce approval.

## Verification

Evidence level: successful GitHub Actions deployments and live HTTP probes,
recorded on 2026-11-14.

- Frozen dependency installation, production build, and Vue typecheck passed.
- All 254 existing frontend tests passed. Wrangler 4.161.0 dry-run read the
  22 production assets and completed without an error.
- The workflow linter passes with one stale action-schema warning excluded:
  `allow-unsafe-pr-checkout` exists in checkout v6's current
  [action definition](https://github.com/actions/checkout/blob/d23441a48e516b6c34aea4fa41551a30e30af803/action.yml).
- [Production deployment](https://github.com/needy-dev/needy/actions/runs/43590003748)
  succeeded. Both production hosts served the homepage, JavaScript, CSS,
  and `/studio` with HTTP 200. Unknown paths served the SPA HTML fallback.
- [Test PR 10](https://github.com/needy-dev/needy/pull/10) added only a temporary
  static marker. Its [prepare run](https://github.com/needy-dev/needy/actions/runs/43590063057)
  succeeded. The deploy job waited for the `Preview` environment review.
- After approval, the [preview deployment](https://github.com/needy-dev/needy/actions/runs/43590080162)
  succeeded and replaced the approval comment with both preview links.
  The version URL and PR alias served the marker and `/studio` HTML.
  Neither production hostname served the marker. The test PR was closed
  without merging, and its branch was deleted.

These probes verify asset delivery, SPA fallback, the approval gate, and preview
separation from production. Browser rendering was not inspected because the
browser control host was unavailable. This run did not exercise a fork PR.
