#!/usr/bin/env sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Assemble the generated parts of the site from the repo's docs/ tree.
# The repo files stay the single source; everything this script writes
# is gitignored. Run automatically by build.sh and serve.sh.
set -eu
cd "$(dirname "$0")"

# Example scenes: embedded into the landing page popups via load_data,
# and served as raw files under /examples/.
mkdir -p static/examples
cp ../docs/examples/*.toml static/examples/

# Repo docs imported as site pages, verbatim except for:
#   - dropping the SPDX comment and the top-level H1 (the template draws it),
#   - rewriting cross-doc links to site URLs (Zola's @/ links, checked at build),
#   - rewriting repo-relative links (source files, diagrams, examples) to GitHub.
import() {
  src="$1"; dst="$2"; title="$3"; weight="$4"; group="$5"; desc="$6"
  {
    printf '+++\ntitle = "%s"\ndescription = "%s"\nweight = %s\ntemplate = "docs-page.html"\n[extra]\ngroup = "%s"\n+++\n\n' \
      "$title" "$desc" "$weight" "$group"
    sed -e '/^<!-- SPDX-License-Identifier:/d' \
        -e '0,/^# /{/^# /d;}' \
        -e 's|](config\.md|](@/docs/configuration.md|g' \
        -e 's|](plugins\.md|](@/docs/plugins/_index.md|g' \
        -e 's|](protocol\.md|](@/docs/protocol.md|g' \
        -e 's|](plugin-api\.md|](@/docs/plugin-api.md|g' \
        -e 's|](architecture\.md|](@/docs/architecture.md|g' \
        -e 's|](plugin-authoring-claude\.md|](@/docs/ai-authoring.md|g' \
        -e 's|](examples/|](https://github.com/sylflo/veiland/blob/master/docs/examples/|g' \
        -e 's|](diagrams/|](https://github.com/sylflo/veiland/blob/master/docs/diagrams/|g' \
        -e 's|](\.\./|](https://github.com/sylflo/veiland/blob/master/|g' \
        "../docs/$src"
  } > "content/docs/$dst"
}

import config.md configuration.md "Configuration" 2 guide \
  "The config.toml schema: plugin entries, z-order, monitors, regions, and the password field."
import protocol.md protocol.md "Protocol" 10 reference \
  "The plugin-to-host wire format: socket transport, messages, and buffer passing."
import plugin-api.md plugin-api.md "Plugin API" 11 reference \
  "The veiland-plugin Rust SDK: connection, frame pacing, and DMA-BUF helpers."
import architecture.md architecture.md "Architecture" 12 reference \
  "How veiland-core, the plugins, and the compositor fit together."
import plugin-authoring-claude.md ai-authoring.md "AI-assisted authoring" 13 reference \
  "Purpose-built context for writing a veiland plugin with a coding assistant."
