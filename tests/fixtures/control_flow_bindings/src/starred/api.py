"""Consumer using star import. Mutations to User must propagate here via
the reverse-import index resolved from the static __all__ in models.py."""
from .models import *  # noqa: F401, F403


def whoami() -> str:
    return User().name()
