"""Tuple unpack inside an if block — exercises walk_control_flow + multi-name StatementBind."""
import sys

if sys.version_info >= (3, 11):
    MAX_RETRIES, BACKOFF = 5, 0.5
else:
    MAX_RETRIES, BACKOFF = 3, 1.0
