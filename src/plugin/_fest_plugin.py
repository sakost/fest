"""fest pytest plugin -- communicates with the fest Rust process over a Unix socket.

This plugin is embedded inside the fest binary and written to a temporary
directory at runtime.  It is registered with pytest via ``-p _fest_plugin``
and expects a ``--fest-socket`` CLI option pointing to the Unix domain
socket that the fest process is listening on.

Protocol (JSON-over-newline):
    fest  ->  plugin:  {"type": "mutant", "file": "...", "module": "...",
                        "mutated_source": "...", "tests": ["..."]}
    plugin ->  fest:   {"type": "result", "status": "killed"|"survived"|"error",
                        "error_message": "..."}
    fest  ->  plugin:  {"type": "shutdown"}
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


def pytest_sessionstart(session: Any) -> None:
    """Connect to the fest socket and enter the mutant-execution loop."""
    socket_path: str | None = session.config.getoption("fest_socket")
    if socket_path is None:
        return

    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        conn.connect(socket_path)
    except OSError as exc:
        print(f"fest: connect failed: {exc}", file=sys.stderr)
        conn.close()
        return

    conn.settimeout(60.0)
    _send(conn, {"type": "ready"})

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
                return
            if msg_type == "mutant":
                result = _handle_mutant(session, msg)
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


def _handle_mutant(session: Any, msg: dict[str, Any]) -> dict[str, Any]:
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

        status = _run_tests(session, test_ids)
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


def _run_tests(session: Any, test_ids: list[str]) -> str:
    """Run the given tests and return ``'killed'`` or ``'survived'``."""
    import pytest  # noqa: PLC0415

    args = ["-x", "--no-header", "-q", "--tb=no"] + test_ids
    exit_code = pytest.main(args, plugins=[])

    if exit_code == 0:
        return "survived"
    return "killed"


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
