"""Tests for fest plugin's ReverseImportIndex class."""

from __future__ import annotations

import sys
import types

import pytest

from _fest_plugin import ReverseImportIndex


@pytest.fixture
def fake_modules():
    created = []

    def factory(name: str, **attrs):
        mod = types.ModuleType(name)
        for key, value in attrs.items():
            setattr(mod, key, value)
        sys.modules[name] = mod
        created.append(name)
        return mod

    yield factory

    for name in created:
        sys.modules.pop(name, None)


def test_runtime_layer_finds_function_imports(fake_modules):
    target_mod = fake_modules("fake_target_pkg")

    def my_func():
        return 1

    my_func.__module__ = "fake_target_pkg"
    my_func.__qualname__ = "my_func"
    target_mod.my_func = my_func

    consumer = fake_modules("fake_consumer_pkg", my_func=my_func)

    idx = ReverseImportIndex.build_runtime_layer()
    hits = idx.lookup("fake_target_pkg", "my_func")

    assert any(d is consumer.__dict__ and key == "my_func" for d, key in hits)


def test_ast_layer_resolves_consumer_to_loaded_dict(fake_modules):
    consumer = fake_modules("fake_consumer_const", MAX=100)
    bindings = [
        {
            "consumer_module": "fake_consumer_const",
            "consumer_key": "MAX",
            "target_module": "fake_target_const",
            "target_name": "MAX",
        }
    ]
    idx = ReverseImportIndex()
    idx.ingest_ast_layer(bindings)
    hits = idx.lookup("fake_target_const", "MAX")

    assert (consumer.__dict__, "MAX") in hits


def test_ast_layer_skips_unloaded_consumers():
    bindings = [
        {
            "consumer_module": "totally_unloaded_xyz",
            "consumer_key": "Q",
            "target_module": "tgt",
            "target_name": "Q",
        }
    ]
    idx = ReverseImportIndex()
    idx.ingest_ast_layer(bindings)
    assert idx.lookup("tgt", "Q") == []


def test_pending_star_import_resolves_with_runtime_all(tmp_path, monkeypatch):
    """When models exposes __all__ at runtime, pending star imports
    synthesize one binding per name."""
    monkeypatch.syspath_prepend(str(tmp_path))
    (tmp_path / "pkg_dyn1").mkdir()
    (tmp_path / "pkg_dyn1" / "__init__.py").write_text("")
    (tmp_path / "pkg_dyn1" / "models_dyn.py").write_text(
        "__all__ = ['A']\nA = 1\nB = 2\n",
    )
    (tmp_path / "pkg_dyn1" / "api_dyn.py").write_text("from pkg_dyn1.models_dyn import *\n")
    import importlib
    importlib.invalidate_caches()
    import pkg_dyn1.api_dyn  # noqa: F401

    idx = ReverseImportIndex()
    idx.ingest_pending_star_imports([
        {"consumer_module": "pkg_dyn1.api_dyn", "target_module": "pkg_dyn1.models_dyn"},
    ])
    # `A` is in __all__ → binding registered. `B` is not.
    assert idx.lookup("pkg_dyn1.models_dyn", "A"), "A in __all__ should be bound"
    assert not idx.lookup("pkg_dyn1.models_dyn", "B"), "B not in __all__"


def test_pending_star_import_falls_back_to_vars_when_all_missing(tmp_path, monkeypatch):
    """No __all__ → register every non-underscore public name."""
    monkeypatch.syspath_prepend(str(tmp_path))
    (tmp_path / "pkg_dyn2").mkdir()
    (tmp_path / "pkg_dyn2" / "__init__.py").write_text("")
    (tmp_path / "pkg_dyn2" / "noall.py").write_text("Open = 1\n_Hidden = 2\n")
    (tmp_path / "pkg_dyn2" / "api.py").write_text("from pkg_dyn2.noall import *\n")
    import importlib
    importlib.invalidate_caches()
    import pkg_dyn2.api  # noqa: F401

    idx = ReverseImportIndex()
    idx.ingest_pending_star_imports([
        {"consumer_module": "pkg_dyn2.api", "target_module": "pkg_dyn2.noall"},
    ])
    assert idx.lookup("pkg_dyn2.noall", "Open"), "non-underscore name should bind"
    assert not idx.lookup("pkg_dyn2.noall", "_Hidden"), \
        "underscore-prefixed should be filtered out"


def test_pending_star_import_silent_skip_on_import_failure():
    """If the source module can't be imported, no bindings, no crash."""
    idx = ReverseImportIndex()
    idx.ingest_pending_star_imports([
        {"consumer_module": "no.such.consumer", "target_module": "no.such.module"},
    ])
    assert not idx.lookup("no.such.module", "anything")
