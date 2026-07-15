#!/usr/bin/env sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Assemble generated content, then serve the site locally.
# Needs zola on PATH; from the repo root: nix develop -c ./site/serve.sh
set -eu
cd "$(dirname "$0")"
./prepare.sh
exec zola serve
