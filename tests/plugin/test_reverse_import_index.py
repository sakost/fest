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
