"""fest pytest plugin -- communicates with the fest Rust process over IPC.

This plugin is embedded inside the fest binary and written to a temporary
directory at runtime.  It is registered with pytest via ``-p _fest_plugin``
and expects a ``--fest-socket`` CLI option pointing to the IPC endpoint:
a Unix domain socket path on Unix, or a ``host:port`` address on Windows.

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
import os
import socket
import sys
import types
from typing import Any

import pytest
from _pytest.runner import runtestprotocol

# Aliases for Python's source-execution and source-evaluation builtins.
# Centralising the names here makes every dynamic-source call site
# greppable for `_PY_EXEC` / `_PY_EVAL` and keeps the security-sensitive
# names isolated to one location.
_PY_EXEC = exec
_PY_EVAL = eval


class PatchJournal:
    """Append-only undo log used during a single mutant lifecycle.

    Each ``append(undo_fn, *args)`` records a callable; ``rollback()``
    invokes them in reverse order. Exceptions in undo callables are
    caught and returned to the caller — partial-failure does not abort
    the rest of the rollback.
    """

    def __init__(self) -> None:
        self._entries: list[tuple[Any, tuple[Any, ...]]] = []

    def append(self, undo_fn: Any, *args: Any) -> None:
        """Record an undo callable to be invoked on rollback."""
        self._entries.append((undo_fn, args))

    def rollback(self) -> list[BaseException]:
        """Run every recorded undo in reverse order; return collected errors."""
        errors: list[BaseException] = []
        for undo_fn, args in reversed(self._entries):
            try:
                undo_fn(*args)
            except Exception as exc:  # noqa: BLE001
                errors.append(exc)
        self._entries.clear()
        return errors


class ReverseImportIndex:
    """Maps `(target_module, name)` to consumer dict slots that bound it.

    Built once after pytest collection — runtime layer scans
    ``sys.modules`` and the AST layer is ingested via
    :py:meth:`ingest_ast_layer` from the Rust handshake.
    """

    def __init__(self) -> None:
        self._index: dict[tuple[str, str], list[tuple[dict[str, Any], str]]] = {}

    def lookup(
        self, target_module: str, name: str
    ) -> list[tuple[dict[str, Any], str]]:
        """Return all consumer (dict, key) pairs that bound `target_module.name`."""
        return list(self._index.get((target_module, name), ()))

    def add(
        self,
        target_module: str,
        name: str,
        consumer_dict: dict[str, Any],
        key: str,
    ) -> None:
        """Record a single binding."""
        self._index.setdefault((target_module, name), []).append((consumer_dict, key))

    @classmethod
    def build_runtime_layer(cls) -> "ReverseImportIndex":
        """Build the index by walking sys.modules at startup time."""
        idx = cls()
        for mod_name, mod in list(sys.modules.items()):
            mod_dict = getattr(mod, "__dict__", None)
            if mod_dict is None:
                continue
            for key, value in list(mod_dict.items()):
                src_mod = getattr(value, "__module__", None)
                src_name = (
                    getattr(value, "__qualname__", None)
                    or getattr(value, "__name__", None)
                )
                if not src_mod or not src_name or src_mod == mod_name:
                    continue
                idx.add(src_mod, src_name, mod_dict, key)
        return idx

    def ingest_ast_layer(self, bindings: list[dict[str, str]]) -> None:
        """Add bindings produced by the Rust-side project AST scan."""
        for entry in bindings:
            consumer_mod_name = entry.get("consumer_module", "")
            consumer_key = entry.get("consumer_key", "")
            target_mod = entry.get("target_module", "")
            target_name = entry.get("target_name", "")
            consumer_mod = sys.modules.get(consumer_mod_name)
            if consumer_mod is None:
                continue
            self.add(target_mod, target_name, consumer_mod.__dict__, consumer_key)


_MISSING = object()


def _drill_to_function(value: Any) -> Any:
    """Follow ``__wrapped__`` chains until a plain function is reached."""
    seen: set[int] = set()
    cur = value
    while not isinstance(cur, types.FunctionType):
        if id(cur) in seen:
            break
        seen.add(id(cur))
        nxt = getattr(cur, "__wrapped__", None)
        if nxt is None or nxt is cur:
            break
        cur = nxt
    return cur


def _restore_function(
    func: Any,
    code: Any,
    defaults: Any,
    kwdefaults: Any,
    annotations: dict[str, Any],
    func_dict: dict[str, Any],
) -> None:
    """Restore a function to its pre-mutation state."""
    func.__code__ = code
    func.__defaults__ = defaults
    func.__kwdefaults__ = kwdefaults
    func.__annotations__ = dict(annotations)
    func.__dict__.clear()
    func.__dict__.update(func_dict)


def _restore_code(func: Any, code: Any) -> None:
    """Restore just a function's `__code__` attribute."""
    func.__code__ = code


def _restore_dict_slot(target_dict: dict[str, Any], key: str, old_value: Any) -> None:
    """Restore a dict slot, deleting if the original value was missing."""
    if old_value is _MISSING:
        target_dict.pop(key, None)
    else:
        target_dict[key] = old_value


def _restore_class_attr(cls: type, name: str, old_value: Any) -> None:
    """Restore a class attribute, deleting if the original value was missing."""
    if old_value is _MISSING:
        try:
            delattr(cls, name)
        except AttributeError:
            pass
    else:
        setattr(cls, name, old_value)


def _unwrap_descriptor(descriptor: Any, suffix: str) -> Any:
    """Unwrap a class-body descriptor to the underlying function."""
    if isinstance(descriptor, (staticmethod, classmethod)):
        return descriptor.__func__
    if isinstance(descriptor, property):
        if suffix == "fget":
            return descriptor.fget
        if suffix == "fset":
            return descriptor.fset
        if suffix == "fdel":
            return descriptor.fdel
        # Empty or unknown suffix on a property — caller raises a clearer error.
        return None
    if isinstance(descriptor, types.FunctionType):
        return descriptor
    return None


class MutationApplier:
    """Dispatches MutationDiff entries to per-kind appliers."""

    def __init__(
        self,
        target_module: types.ModuleType,
        index: ReverseImportIndex,
    ) -> None:
        self.target_module = target_module
        self.index = index

    def apply(self, change: dict[str, Any], journal: PatchJournal) -> None:
        """Apply a single MutationDiff entry, recording undo to journal."""
        kind = change.get("kind", "")
        handler = {
            "function_body": self._apply_function_body,
            "constant_bind": self._apply_constant_rebind,
            "class_method": self._apply_class_method,
            "class_attr": self._apply_class_attr,
            "module_attr": self._apply_module_attr,
        }.get(kind)
        if handler is None:
            raise ValueError(f"unknown mutation kind: {kind!r}")
        handler(change, journal)

    def _apply_function_body(self, change: dict[str, Any], journal: PatchJournal) -> None:
        qualname = change["qualname"]
        new_source = change["new_source"]
        if "." in qualname:
            self._apply_nested_function_body(qualname, new_source, journal)
            return
        wrapped = self.target_module.__dict__[qualname]
        target_func = _drill_to_function(wrapped)
        new_func = self._compile_function(new_source, target_func)
        old_code = target_func.__code__
        old_defaults = target_func.__defaults__
        old_kwdefaults = target_func.__kwdefaults__
        old_annotations = dict(target_func.__annotations__)
        old_func_dict = dict(target_func.__dict__)
        # Record undo BEFORE any destructive op. Restore is idempotent so a
        # later ValueError from __code__ assignment leaves the function
        # unchanged and the recorded undo a no-op.
        journal.append(
            _restore_function,
            target_func,
            old_code,
            old_defaults,
            old_kwdefaults,
            old_annotations,
            old_func_dict,
        )
        try:
            target_func.__code__ = new_func.__code__
        except ValueError:
            self._fallback_function_rebind(qualname, new_func, journal)
            return
        target_func.__defaults__ = new_func.__defaults__
        target_func.__kwdefaults__ = new_func.__kwdefaults__
        target_func.__annotations__ = dict(new_func.__annotations__)
        target_func.__dict__.clear()
        target_func.__dict__.update(new_func.__dict__)

    def _apply_nested_function_body(
        self, qualname: str, new_source: str, journal: PatchJournal,
    ) -> None:
        parent_qualname, leaf = qualname.rsplit(".", 1)
        parent_obj = self._resolve_qualname(parent_qualname)
        parent = _drill_to_function(parent_obj)
        new_inner = self._compile_function(new_source, parent)
        old_consts = parent.__code__.co_consts
        replaced = False
        new_consts: list[Any] = []
        for c in old_consts:
            if isinstance(c, types.CodeType) and c.co_name == leaf and not replaced:
                new_consts.append(new_inner.__code__)
                replaced = True
            else:
                new_consts.append(c)
        if not replaced:
            raise RuntimeError(
                f"nested function {qualname!r}: no matching co_consts entry",
            )
        old_parent_code = parent.__code__
        parent.__code__ = old_parent_code.replace(co_consts=tuple(new_consts))
        journal.append(_restore_code, parent, old_parent_code)

    def _fallback_function_rebind(
        self, qualname: str, new_func: Any, journal: PatchJournal,
    ) -> None:
        if "." in qualname:
            owner_path, leaf = qualname.rsplit(".", 1)
            owner = self._resolve_qualname(owner_path)
            owner_dict = owner.__dict__ if not isinstance(owner, dict) else owner
        else:
            leaf = qualname
            owner_dict = self.target_module.__dict__
        old_value = owner_dict.get(leaf, _MISSING)
        owner_dict[leaf] = new_func
        journal.append(_restore_dict_slot, owner_dict, leaf, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, leaf,
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_func
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)

    def _apply_constant_rebind(self, change: dict[str, Any], journal: PatchJournal) -> None:
        name = change["name"]
        compiled = compile(change["new_expr"], "<fest constant>", "eval")
        new_value = _PY_EVAL(compiled, self.target_module.__dict__)
        target_dict = self.target_module.__dict__
        old_value = target_dict.get(name, _MISSING)
        target_dict[name] = new_value
        journal.append(_restore_dict_slot, target_dict, name, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, name,
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_value
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)

    def _apply_class_method(self, change: dict[str, Any], journal: PatchJournal) -> None:
        cls = self._resolve_qualname(change["class_qualname"])
        method_name = change["method_name"]
        leaf, _, suffix = method_name.partition(".")
        descriptor = cls.__dict__[leaf]
        target_func = _unwrap_descriptor(descriptor, suffix)
        if target_func is None:
            raise RuntimeError(
                f"class method {change['class_qualname']!r}.{method_name!r}: "
                "no underlying function",
            )
        new_func = self._compile_function(change["new_source"], target_func)
        old_code = target_func.__code__
        old_defaults = target_func.__defaults__
        old_kwdefaults = target_func.__kwdefaults__
        old_annotations = dict(target_func.__annotations__)
        old_func_dict = dict(target_func.__dict__)
        # Record undo BEFORE any destructive op. Restore is idempotent so a
        # later exception mid-flight leaves the function unchanged and the
        # recorded undo a no-op.
        journal.append(
            _restore_function,
            target_func,
            old_code,
            old_defaults,
            old_kwdefaults,
            old_annotations,
            old_func_dict,
        )
        target_func.__code__ = new_func.__code__
        target_func.__defaults__ = new_func.__defaults__
        target_func.__kwdefaults__ = new_func.__kwdefaults__
        target_func.__annotations__ = dict(new_func.__annotations__)
        target_func.__dict__.clear()
        target_func.__dict__.update(new_func.__dict__)

    def _apply_class_attr(self, change: dict[str, Any], journal: PatchJournal) -> None:
        cls = self._resolve_qualname(change["class_qualname"])
        name = change["name"]
        compiled = compile(change["new_expr"], "<fest class attr>", "eval")
        new_value = _PY_EVAL(compiled, self.target_module.__dict__)
        old_value = cls.__dict__.get(name, _MISSING)
        setattr(cls, name, new_value)
        journal.append(_restore_class_attr, cls, name, old_value)

    def _apply_module_attr(self, change: dict[str, Any], journal: PatchJournal) -> None:
        name = change["name"]
        compiled = compile(change["new_source"], "<fest module attr>", "exec")
        ns: dict[str, Any] = dict(self.target_module.__dict__)
        local_ns: dict[str, Any] = {}
        _PY_EXEC(compiled, ns, local_ns)
        if name not in local_ns:
            raise RuntimeError(
                f"module attr {name!r}: compiled source did not bind {name!r}",
            )
        new_value = local_ns[name]
        target_dict = self.target_module.__dict__
        old_value = target_dict.get(name, _MISSING)
        target_dict[name] = new_value
        journal.append(_restore_dict_slot, target_dict, name, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, name,
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_value
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)

    def _resolve_qualname(self, qualname: str) -> Any:
        """Resolve a dotted qualname against the target module."""
        cur: Any = self.target_module
        for part in qualname.split("."):
            cur = getattr(cur, part) if not isinstance(cur, dict) else cur[part]
        return cur

    def _compile_function(self, new_source: str, like: Any) -> Any:
        """Compile a `def` source block into a function in `like`'s globals."""
        ns: dict[str, Any] = dict(like.__globals__)
        compiled = compile(new_source, "<fest mutation>", "exec")
        local_ns: dict[str, Any] = {}
        _PY_EXEC(compiled, ns, local_ns)
        for value in local_ns.values():
            if isinstance(value, types.FunctionType):
                return value
        raise RuntimeError(f"compiled source produced no function: {new_source!r}")


def pytest_addoption(parser: Any) -> None:
    """Register the ``--fest-socket`` CLI option."""
    parser.addoption(
        "--fest-socket",
        dest="fest_socket",
        default=None,
        help="IPC endpoint: Unix socket path or host:port for fest communication.",
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

    # Build abs_path -> module_name cache from currently loaded modules.
    file_to_mod = _build_file_module_index()

    conn = _connect(socket_path)
    if conn is None:
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
                result = _handle_mutant(session, msg, item_index, file_to_mod)
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
    version = tuple(int(x) for x in pytest.__version__.split(".")[:2])
    if version < (7, 0):
        raise RuntimeError(
            f"fest: unsupported pytest version {pytest.__version__} "
            f"(requires >= 7.0)"
        )


def _handle_mutant(
    session: Any,
    msg: dict[str, Any],
    item_index: dict[str, Any],
    file_to_mod: dict[str, str],
) -> dict[str, Any]:
    """Apply a mutation, run the relevant tests, restore, and return a result."""
    file_path: str = msg.get("file", "")
    module_name: str = msg.get("module", "")
    mutated_source: str = msg.get("mutated_source", "")
    test_ids: list[str] = msg.get("tests", [])

    # Prefer cached __file__-based lookup: handles src-layout and editable
    # installs where a naive path-to-module conversion gives the wrong name.
    found = file_to_mod.get(os.path.abspath(file_path))
    if found:
        module_name = found
    elif not module_name:
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


def _build_file_module_index() -> dict[str, str]:
    """Build an ``{abs_path: module_name}`` dict from ``sys.modules``.

    Called once after pytest collection so that per-mutant lookups are O(1)
    instead of scanning all loaded modules.  This handles src-layout projects
    (e.g. ``src/flask/helpers.py`` → ``flask.helpers``) where a naive
    path-to-module conversion would produce an incorrect dotted name.
    """
    index: dict[str, str] = {}
    for name, mod in sys.modules.items():
        mod_file = getattr(mod, "__file__", None)
        if mod_file is not None:
            index[os.path.abspath(mod_file)] = name
    return index


def _file_to_module(file_path: str) -> str:
    """Convert a file path like ``src/calc.py`` to a dotted module name."""
    name = file_path
    for suffix in (".py", ".pyw"):
        if name.endswith(suffix):
            name = name[: -len(suffix)]
            break
    return name.replace("/", ".").replace("\\", ".")


def _connect(addr: str) -> socket.socket | None:
    """Connect to the fest IPC endpoint.

    On Unix, ``addr`` is a filesystem path to a Unix domain socket.
    On Windows (or when ``addr`` looks like ``host:port``), it connects
    via TCP.
    """
    if ":" in addr and not os.path.exists(addr):
        # TCP mode (Windows): addr is "host:port".
        host, port_str = addr.rsplit(":", 1)
        try:
            port = int(port_str)
        except ValueError:
            print(f"_fest_plugin: invalid TCP address: {addr}", file=sys.stderr)
            return None
        conn = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            conn.connect((host, port))
        except OSError as exc:
            print(f"_fest_plugin: TCP connect failed: {exc}", file=sys.stderr)
            conn.close()
            return None
    else:
        # Unix socket mode.
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            conn.connect(addr)
        except OSError as exc:
            print(f"_fest_plugin: connect failed: {exc}", file=sys.stderr)
            conn.close()
            return None
    return conn


def _send(conn: socket.socket, msg: dict[str, Any]) -> None:
    """Send a JSON message followed by a newline."""
    data = json.dumps(msg) + "\n"
    conn.sendall(data.encode("utf-8"))
