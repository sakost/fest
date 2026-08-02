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


def test_statement_bind_updates_target_and_consumer(target_module):
    target_module.MAX = 100
    consumer = {"MAX": 100}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "MAX", consumer, "MAX")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {
            "kind": "statement_bind",
            "names": ["MAX"],
            "stmt_source": "MAX = 101",
            "scope": {"kind": "module"},
        },
        journal,
    )

    assert target_module.MAX == 101
    assert consumer["MAX"] == 101

    journal.rollback()
    assert target_module.MAX == 100
    assert consumer["MAX"] == 100


def test_class_method_plain_swap(target_module):
    src = "class Calc:\n    def add(self, a, b):\n        return a + b\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    Calc = target_module.Calc

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "Calc",
            "method_name": "add",
            "new_source": "def add(self, a, b):\n    return a - b\n",
        },
        journal,
    )

    assert Calc().add(5, 3) == 2
    journal.rollback()
    assert Calc().add(5, 3) == 8


def test_class_method_staticmethod_swap(target_module):
    src = "class C:\n    @staticmethod\n    def k():\n        return 1\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "k",
            "new_source": "def k():\n    return 2\n",
        },
        journal,
    )

    assert C.k() == 2
    journal.rollback()
    assert C.k() == 1


def test_class_method_classmethod_swap(target_module):
    src = "class C:\n    @classmethod\n    def m(cls):\n        return 1\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "m",
            "new_source": "def m(cls):\n    return 2\n",
        },
        journal,
    )

    assert C.m() == 2
    journal.rollback()
    assert C.m() == 1


def test_property_fget_mutation(target_module):
    src = (
        "class C:\n"
        "    @property\n"
        "    def x(self):\n"
        "        return 1\n"
    )
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "x.fget",
            "new_source": "def x(self):\n    return 2\n",
        },
        journal,
    )

    assert C().x == 2
    journal.rollback()
    assert C().x == 1


def test_statement_bind_class_scope_simple(target_module):
    src = "class C:\n    LIMIT = 10\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier._apply_statement_bind(
        {
            "kind": "statement_bind",
            "names": ["LIMIT"],
            "stmt_source": "LIMIT = 11",
            "scope": {"kind": "class", "qualname": "C"},
        },
        journal,
    )

    assert C.LIMIT == 11
    journal.rollback()
    assert C.LIMIT == 10


def test_fallback_function_rebind_replaces_target_and_consumers(target_module):
    """Direct test of _fallback_function_rebind (the closure-mismatch fallback path).

    `__code__`-swap raises `ValueError` when the new code object has a
    different `co_freevars` count from the original. In that branch
    `_apply_function_body` calls `_fallback_function_rebind` to replace
    the function object outright via the reverse-import index. We test
    the fallback method directly because constructing the exact bytecode
    pattern that triggers it through the public `function_body` change
    dict would require hand-crafting code objects.
    """
    src = "def foo(x):\n    return x + 1\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    target_module.foo.__module__ = target_module.__name__
    consumer_a = {"foo": target_module.foo}
    consumer_b = {"renamed_foo": target_module.foo}

    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "foo", consumer_a, "foo")
    idx.add(target_module.__name__, "foo", consumer_b, "renamed_foo")

    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    # New function object — different identity, different behavior.
    def new_foo(x):
        return x - 1

    new_foo.__module__ = target_module.__name__
    new_foo.__qualname__ = "foo"

    applier._fallback_function_rebind("foo", new_foo, journal)

    # Target module's slot now points to new_foo.
    assert target_module.foo(5) == 4
    # Both consumer slots also point to new_foo (identity-breaking rebind).
    assert consumer_a["foo"](5) == 4
    assert consumer_b["renamed_foo"](5) == 4

    journal.rollback()

    # All slots restored.
    assert target_module.foo(5) == 6
    assert consumer_a["foo"](5) == 6
    assert consumer_b["renamed_foo"](5) == 6


def test_module_attr_rebind_runs_def_block(target_module):
    target_module.foo = lambda: 1
    target_module.foo.__module__ = target_module.__name__
    consumer = {"foo": target_module.foo}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "foo", consumer, "foo")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {
            "kind": "module_attr",
            "name": "foo",
            "new_source": "def foo():\n    return 2\n",
        },
        journal,
    )

    assert target_module.foo() == 2
    assert consumer["foo"]() == 2

    journal.rollback()
    assert target_module.foo() == 1
    assert consumer["foo"]() == 1


def test_apply_with_no_changes_is_noop(target_module):
    """Empty diff list should be a no-op — apply() is never called."""
    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()

    # Iterate empty list → no calls
    for change in []:
        applier.apply(change, journal)

    # Journal is empty
    assert journal.rollback() == []


def test_statement_bind_aug_assign_uses_current_value(target_module):
    """AugAssign reads the prior value from the namespace at exec time."""
    target_module.COUNTER = 5
    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    change = {
        "kind": "statement_bind",
        "names": ["COUNTER"],
        "stmt_source": "COUNTER += 2",
        "scope": {"kind": "module"},
    }
    applier.apply(change, journal)
    assert target_module.COUNTER == 7  # 5 + 2 — reads current namespace value
    journal.rollback()
    assert target_module.COUNTER == 5


@pytest.fixture
def applier_setup(target_module):
    """Return (applier, module, journal) bound to a fresh target module."""
    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    return applier, target_module, journal


def test_journal_restores_first_change_when_second_apply_raises(target_module):
    target_module.MAX = 100
    consumer = {"MAX": 100}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "MAX", consumer, "MAX")

    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {
            "kind": "statement_bind",
            "names": ["MAX"],
            "stmt_source": "MAX = 101",
            "scope": {"kind": "module"},
        },
        journal,
    )
    assert target_module.MAX == 101

    with pytest.raises(SyntaxError):
        applier.apply(
            {
                "kind": "statement_bind",
                "names": ["MAX"],
                "stmt_source": "MAX = ((",
                "scope": {"kind": "module"},
            },
            journal,
        )

    journal.rollback()
    assert target_module.MAX == 100
    assert consumer["MAX"] == 100


def test_statement_bind_multi_name(applier_setup):
    """Tuple unpack at module scope binds both names; rollback restores both."""
    applier, module, journal = applier_setup
    module.a = 99   # pre-state — verify journal restores
    # 'b' is intentionally unset, so journal records _MISSING and rollback deletes.
    change = {
        "kind": "statement_bind",
        "names": ["a", "b"],
        "stmt_source": "a, b = 1, 3",
        "scope": {"kind": "module"},
    }
    applier._apply_statement_bind(change, journal)
    assert module.a == 1
    assert module.b == 3
    journal.rollback()
    assert module.a == 99
    assert not hasattr(module, "b"), "b was _MISSING pre-mutation; rollback should delete"


def test_statement_bind_class_scope_exec_uses_module_globals(applier_setup):
    """Class body sees module globals during exec — needed for forward refs."""
    applier, module, journal = applier_setup
    module.HELPER = lambda: 7
    class C:
        pass
    module.C = C
    change = {
        "kind": "statement_bind",
        "names": ["X"],
        "stmt_source": "X = HELPER()",
        "scope": {"kind": "class", "qualname": "C"},
    }
    applier._apply_statement_bind(change, journal)
    assert C.X == 7
    journal.rollback()
    assert not hasattr(C, "X")


def test_statement_bind_class_scope_missing_class_raises_skipped(applier_setup):
    """If the class qualname doesn't resolve, raise _SkippedMutation."""
    from _fest_plugin import _SkippedMutation
    applier, module, journal = applier_setup
    change = {
        "kind": "statement_bind",
        "names": ["X"],
        "stmt_source": "X = 1",
        "scope": {"kind": "class", "qualname": "NotARealClass"},
    }
    try:
        applier._apply_statement_bind(change, journal)
        raised = False
    except _SkippedMutation as exc:
        raised = True
        assert exc.reason == "missing_class_scope"
    assert raised, "expected _SkippedMutation for missing class"


def test_statement_bind_class_scope_aug_assign_reads_current_value(applier_setup):
    """AugAssign inside a class reads the current class attribute."""
    applier, module, journal = applier_setup
    class C:
        counter = 5
    module.C = C
    change = {
        "kind": "statement_bind",
        "names": ["counter"],
        "stmt_source": "counter += 2",
        "scope": {"kind": "class", "qualname": "C"},
    }
    applier._apply_statement_bind(change, journal)
    assert C.counter == 7
    journal.rollback()
    assert C.counter == 5


def test_handle_mutant_errors_when_no_test_ids_match(target_module):
    """A total test-id mismatch must surface as an error, not 'survived'.

    Regression: pytest rootdir drift once made every worker nodeid carry a
    path prefix, so no runner-sent test id matched and every mutant was
    silently reported as survived.
    """
    from _fest_plugin import _handle_mutant

    msg = {
        "file": "src/applier_target_mod.py",
        "module": target_module.__name__,
        "diff": [{"kind": "statement_bind", "names": ["X"],
                  "stmt_source": "X = 6", "scope": {"kind": "module"}}],
        "tests": ["tests/test_a.py::test_one"],
    }
    item_index = {"prefix/tests/test_a.py::test_one": object()}
    result = _handle_mutant(
        None, msg, item_index, {}, ReverseImportIndex(),
    )
    assert result["status"] == "error"
    assert "matched" in result["error_message"]
