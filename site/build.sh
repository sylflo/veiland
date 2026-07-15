#!/usr/bin/env sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Assemble generated content, then build the site into public/.
# Needs zola on PATH; from the repo root: nix develop -c ./site/build.sh
set -eu
cd "$(dirname "$0")"
./prepare.sh
exec zola build "$@"
