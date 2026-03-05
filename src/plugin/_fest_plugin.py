"""fest pytest plugin -- communicates with the fest Rust process over a Unix socket.

This plugin is embedded inside the fest binary and written to a temporary
directory at runtime.  It is registered with pytest via ``-p _fest_plugin``
and expects a ``--fest-socket`` CLI option pointing to the Unix domain
socket that the fest process is listening on.

Protocol (JSON-over-newline):
    plugin ->  fest:   {"type": "ready", "tests": ["nodeid", ...]}
    fest  ->  plugin:  {"type": "mutant", "file": "...", "module": "...",
                        "mutated_source": "...", "tests": ["..."]}
    plugin ->  fest:   {"type": "result", "status": "killed"|"survived"|"error",
                        "error_message": "..."}
    fest  ->  plugin:  {"type": "shutdown"}

Requires pytest >= 7.0.
"""

from __future__ import annotations

import json
import socket
import sys
import types
from typing import Any


def pytest_addoption(parser: Any) -> None:
    """Register the ``--fest-socket`` CLI option."""
    parser.addoption(
        "--fest-socket",
        dest="fest_socket",
        default=None,
        help="Path to the Unix domain socket for fest communication.",
    )


def pytest_runtestloop(session: Any) -> bool:
    """Run the fest event loop after collection, replacing the default test loop.

    Returns ``True`` to tell pytest we handled test execution ourselves.
    """
    socket_path: str | None = session.config.getoption("fest_socket")
    if socket_path is None:
        return False

    _check_pytest_version()

    # Build nodeid -> Item index from collected items.
    item_index: dict[str, Any] = {}
    for item in session.items:
        item_index[item.nodeid] = item

    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        conn.connect(socket_path)
    except OSError as exc:
        print(f"fest: connect failed: {exc}", file=sys.stderr)
        conn.close()
        return True

    # Use a generous timeout; the Rust side enforces per-mutant timeouts.
    conn.settimeout(None)
    test_ids = [item.nodeid for item in session.items]
    _send(conn, {"type": "ready", "tests": test_ids})

    buf = b""
    while True:
        chunk = conn.recv(4096)
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError as exc:
                _send(
                    conn,
                    {
                        "type": "result",
                        "status": "error",
                        "error_message": f"bad json: {exc}",
                    },
                )
                continue

            msg_type = msg.get("type", "")
            if msg_type == "shutdown":
                conn.close()
                return True
            if msg_type == "mutant":
                result = _handle_mutant(session, msg, item_index)
                _send(conn, result)
            else:
                _send(
                    conn,
                    {
                        "type": "result",
                        "status": "error",
                        "error_message": f"unknown type: {msg_type}",
                    },
                )

    conn.close()
    return True


def _check_pytest_version() -> None:
    """Verify that the pytest version is supported (>= 7.0).

    Raises ``RuntimeError`` if the version is outside the supported range,
    which causes the connection to fail and triggers the Rust-side subprocess
    fallback.
    """
    import pytest  # noqa: PLC0415

    version = tuple(int(x) for x in pytest.__version__.split(".")[:2])
    if version < (7, 0):
        raise RuntimeError(
            f"fest: unsupported pytest version {pytest.__version__} "
            f"(requires >= 7.0)"
        )


def _handle_mutant(
    session: Any, msg: dict[str, Any], item_index: dict[str, Any]
) -> dict[str, Any]:
    """Apply a mutation, run the relevant tests, restore, and return a result."""
    file_path: str = msg.get("file", "")
    module_name: str = msg.get("module", "")
    mutated_source: str = msg.get("mutated_source", "")
    test_ids: list[str] = msg.get("tests", [])

    if not module_name:
        module_name = _file_to_module(file_path)

    original_module = sys.modules.get(module_name)
    saved_dict: dict[str, Any] | None = None

    try:
        code = compile(mutated_source, file_path, "exec")
    except SyntaxError as exc:
        return {
            "type": "result",
            "status": "error",
            "error_message": f"compile error: {exc}",
        }

    try:
        if original_module is not None:
            saved_dict = dict(vars(original_module))
            _exec_code(code, vars(original_module))
        else:
            mod = types.ModuleType(module_name)
            mod.__file__ = file_path
            sys.modules[module_name] = mod
            _exec_code(code, vars(mod))

        status = _run_tests(session, test_ids, item_index)
        return {"type": "result", "status": status}
    except Exception as exc:  # noqa: BLE001
        return {
            "type": "result",
            "status": "error",
            "error_message": f"runtime error: {exc}",
        }
    finally:
        if original_module is not None and saved_dict is not None:
            vars(original_module).clear()
            vars(original_module).update(saved_dict)
        elif original_module is None and module_name in sys.modules:
            del sys.modules[module_name]


def _exec_code(code: types.CodeType, namespace: dict[str, Any]) -> None:
    """Execute compiled code in the given namespace.

    Separated into its own function to keep the security-sensitive call
    isolated and auditable.
    """
    # This is intentional: fest needs to load mutated Python source code
    # into the target module's namespace for mutation testing.
    glob = namespace  # noqa: A001
    exec(code, glob)  # noqa: S102  -- required for mutation testing


def _run_tests(
    session: Any, test_ids: list[str], item_index: dict[str, Any]
) -> str:
    """Run the given tests via ``runtestprotocol`` and return ``'killed'`` or ``'survived'``."""
    from _pytest.runner import runtestprotocol  # noqa: PLC0415

    items = []
    for nodeid in test_ids:
        item = item_index.get(nodeid)
        if item is not None:
            items.append(item)

    if not items:
        return "survived"

    for idx, item in enumerate(items):
        nextitem = items[idx + 1] if idx + 1 < len(items) else None
        reports = runtestprotocol(item, log=False, nextitem=nextitem)
        for report in reports:
            if report.when in ("setup", "call") and report.failed:
                return "killed"

    return "survived"


def _file_to_module(file_path: str) -> str:
    """Convert a file path like ``src/calc.py`` to a dotted module name."""
    name = file_path
    for suffix in (".py", ".pyw"):
        if name.endswith(suffix):
            name = name[: -len(suffix)]
            break
    return name.replace("/", ".").replace("\\", ".")


def _send(conn: socket.socket, msg: dict[str, Any]) -> None:
    """Send a JSON message followed by a newline."""
    data = json.dumps(msg) + "\n"
    conn.sendall(data.encode("utf-8"))
