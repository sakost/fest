"""Make the embedded fest plugin importable from unit tests.

`_fest_plugin.py` lives at `src/plugin/_fest_plugin.py` because the
Rust build embeds it via include_str!. For Python-side unit testing
we put that directory on sys.path here.
"""

from __future__ import annotations

import sys
from pathlib import Path

_PLUGIN_DIR = Path(__file__).resolve().parents[2] / "src" / "plugin"
if str(_PLUGIN_DIR) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_DIR))
