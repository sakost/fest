from src.starred.api import whoami
from src.starred.models import User


def test_whoami_returns_alice():
    assert whoami() == "alice"


def test_user_export():
    assert User().name() == "alice"
