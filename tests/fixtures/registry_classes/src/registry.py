"""Registry-pattern classes exercising __init_subclass__ stability."""


class Plugin:
    registry: list[type] = []

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        Plugin.registry.append(cls)

    def value(self) -> int:
        return 1


class A(Plugin):
    def value(self) -> int:
        return 1
