# @needy/docs

The needy explainer: a guide to what needy is and how a run routes commands,
files and requests, with a setup wizard, a playground that routes your own
commands, a profile studio at `/studio` that checks a profile as needy does,
and the README figures.
Its layout and banner pipeline follow the
[injeca Docs](https://github.com/moeru-ai/injeca/tree/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs).

- Styles are UnoCSS utilities in the templates (`uno.config.ts`): the theme's
  colors are the custom properties of `src/palette.ts` and `src/style.css`,
  `bg-actor/12` mixes a color with transparent, icons are `i-<set>:<name>`
  classes for the names in `src/icons.ts`, and the page's keyframes live in
  the config. Components have no `<style>` blocks.
- `src/scenes`: one Vue component per scene, each a list of steps; `sizes.ts`
  lists them with their design sizes, which are the banner sizes (2.5:1, 2:1,
  3:1).
- `src/setup`: the environment wizard generates steps and a profile for the
  selected entrypoint, actor host, driver, and executor. Its profile templates
  come from `examples/linux-mac/`. Each command block names one terminal context.
- `src/components`: the guide's sections; `src/content.ts` holds their text
  in English and Chinese.
- `src/kit`: what scenes are made of: the frame and its player, boxes, wires,
  packets, a terminal, a YAML view and the trace strip.
- `src/route`: a port of `needy_core::pattern` and the rules' decision, with
  the cases of `crates/core/src/pattern_test.rs`. The routing scene and the
  playground decide with it, and `network.ts` ports the gateway's allowlist.
  When `pattern.rs`, `rules/mod.rs` or `network.rs` changes, change these too.
- `src/studio`: the profile studio, starting with the Linux-to-Mac bash example;
  `schema.ts` and `check.ts` port the
  profile schema and checks of `crates/config`, and change with it.
- `src/data/drivers.ts`: the actor drivers table, from the README.
- `scripts/banners.ts`: renders each scene to `docs/assets/banners` as a
  looping WebP in both themes, through headless Chrome's screencast.

The outputs in the scenes follow what the CLI prints (`crates/cli`,
`crates/core/src/report`) and the closed-loop results; run IDs, durations and
programs' own output are made up for the scene.

```sh
pnpm install
pnpm dev                    # the page
pnpm test                   # the matcher and rules port
pnpm typecheck
pnpm build                  # the static site in dist/
pnpm banners                # all banners; BANNERS=hero,routing for some
STILLS=1 pnpm banners       # a PNG of the end of each step, in .tmp/banners/
```

`pnpm banners` needs Google Chrome (or `CHROME=/path/to/chrome`) and
`img2webp` (`brew install webp`).

## Cloudflare Workers

The site deploys to <https://needy.ayaka.io/> and
<https://needy.ayakamoe.workers.dev/>. `wrangler.toml` serves `dist/` with an
SPA fallback for `/studio`.

The production and PR preview workflows follow airi. The repository needs
`CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets. The `Preview`
GitHub environment requires maintainer approval before it builds PR code.
See the [deployment reference](../../docs/ai/references/architecture/2026-11-14-cloudflare-docs-deployment-design.md)
for the workflow split and verification evidence.
