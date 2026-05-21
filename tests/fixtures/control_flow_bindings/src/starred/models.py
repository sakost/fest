"""Star import source with static __all__."""
__all__ = ["User"]


class User:
    def name(self) -> str:
        return "alice"


class _Internal:
    """Should NOT be exported via `from .models import *`."""
