"""Tests for fest plugin's MutationApplier class."""

from __future__ import annotations

import sys
import types

import pytest

from _fest_plugin import MutationApplier, PatchJournal, ReverseImportIndex


@pytest.fixture
def target_module():
    name = "applier_target_mod"
    mod = types.ModuleType(name)
    sys.modules[name] = mod
    yield mod
    sys.modules.pop(name, None)


def test_apply_raises_on_unknown_kind(target_module):
    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    with pytest.raises(ValueError, match="unknown mutation kind"):
        applier.apply({"kind": "bogus"}, journal)
