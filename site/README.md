# veiland website

The veiland site, built with [Zola](https://www.getzola.org/) and a sprinkle of
[Alpine.js](https://alpinejs.dev/) (vendored in `static/alpine.min.js`, no Node
toolchain). Deployed to GitHub Pages by `.github/workflows/site.yml` on pushes
to `master`.

## Local development

```sh
# from the repo root; zola is in the flake dev shell:
nix develop -c ./site/serve.sh

# anywhere else with zola installed:
./site/serve.sh
```

`serve.sh` and `build.sh` first run `prepare.sh`, which assembles the
generated parts of the site (see below), then run zola.

## Single-source rules

Nothing on the site is written twice. Every piece of content has exactly one
home, and generation bridges the rest:

- **Repo docs are imported, never copied by hand.** `prepare.sh` turns
  `docs/config.md`, `docs/protocol.md`, `docs/plugin-api.md`,
  `docs/architecture.md`, and `docs/plugin-authoring-claude.md` into site
  pages at build time (frontmatter prepended, cross-links rewritten). The
  imported pages are gitignored; edit the repo file.
- **The plugin reference flows the other way.** The structured per-plugin
  files in `content/docs/plugins/*.md` are the source of truth: their
  frontmatter (`[[extra.props]]`) feeds the landing cards, the property
  popups, the docs sidebar, and the `/docs/plugins/<name>/` pages. The
  repo-facing `docs/plugins.md` is **generated** from them by
  `scripts/gen-plugins-md.py`; CI fails if it is out of sync. Shared prose
  (conventions, pitfalls) lives in `content/docs/plugins/_index.md`.
- **Example scenes** are copied from `docs/examples/` by `prepare.sh` and
  embedded into the landing popups via `load_data`.
- **Hand-written site-only pages**: the landing page, getting-started, and
  writing-plugins. These are guides, not references; they link into the
  imported reference pages rather than restating them.

After editing plugin content, regenerate the repo doc:

```sh
python3 scripts/gen-plugins-md.py
```

## Layout

- `templates/index.html`: the landing page.
- `templates/docs-page.html`: docs layout with the left sidebar and
  on-this-page TOC; renders a property table when the page has
  `extra.props`.
- **Previews are real captures.** The README gallery GIFs are copied in by
  `prepare.sh` (single source: `docs/assets/readme/`); the other captures
  live in `static/previews/`. A plugin page shows whatever its frontmatter's
  `image = "previews/..."` points at: mp4/webm render as looping video,
  anything else as an image. A page without an `image` simply has no media,
  so a new plugin's page works before its capture is recorded
  (`scripts/record/scene.sh` and the configs in `scripts/record/showcase/`
  are the capture pipeline).
- `static/landing.js`: the config-popup helper.

Keep site prose free of em dashes. Text imported or generated from repo docs
is exempt; it stays verbatim.

## Publishing checklist (first deploy)

1. Merge to `master`.
2. In the GitHub repo settings, set Pages > Source to "GitHub Actions".
3. The workflow builds with the `base_url` from `config.toml`
   (`https://sylflo.github.io/veiland`).

## Not done yet

- Real capture videos in the slider, scenes gallery, and plugin cards.
- Scene pages under `/docs/` (the landing popups cover scenes for now).
