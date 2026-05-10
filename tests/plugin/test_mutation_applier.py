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


def test_function_body_preserves_identity_after_mutation(target_module):
    src = "def foo(x):\n    return x + 1\n"
    compiled = compile(src, "<test>", "exec")
    exec(compiled, target_module.__dict__)
    target_module.foo.__module__ = target_module.__name__
    foo_id = id(target_module.foo)
    consumer = {"foo": target_module.foo}

    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "foo", consumer, "foo")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    change = {
        "kind": "function_body",
        "qualname": "foo",
        "new_source": "def foo(x):\n    return x - 1\n",
    }
    applier.apply(change, journal)

    assert target_module.foo(5) == 4
    assert id(target_module.foo) == foo_id
    assert consumer["foo"](5) == 4

    journal.rollback()
    assert target_module.foo(5) == 6


def test_nested_function_body_via_co_consts(target_module):
    src = (
        "def outer():\n"
        "    def inner():\n"
        "        return 1\n"
        "    return inner\n"
    )
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    target_module.outer.__module__ = target_module.__name__
    outer_id = id(target_module.outer)

    idx = ReverseImportIndex()
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    change = {
        "kind": "function_body",
        "qualname": "outer.inner",
        "new_source": "def inner():\n    return 2\n",
    }
    applier.apply(change, journal)

    inner = target_module.outer()
    assert inner() == 2
    assert id(target_module.outer) == outer_id

    journal.rollback()
    assert target_module.outer()() == 1
