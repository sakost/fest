"""AnnAssign at module scope + tuple ann assign with typed annotation."""
COUNTER: int = 0
HEADERS: tuple = ("content-type", "accept")


def reset() -> int:
    global COUNTER
    COUNTER = 0
    return COUNTER
