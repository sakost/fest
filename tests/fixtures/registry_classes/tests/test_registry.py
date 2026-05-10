from src.registry import A, Plugin


def test_a_value():
    assert A().value() == 1


def test_registry_size_stable():
    assert len(Plugin.registry) == 1
