# SPDX-License-Identifier: GPL-3.0-or-later
#
# Put the single-file SDK (python/veiland_plugin.py) on the import path so
# the tests find it without packaging or an installed wheel -- vendoring is
# the distribution model, so the test setup matches how authors use it.

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
