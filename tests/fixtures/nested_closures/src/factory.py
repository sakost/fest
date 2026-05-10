"""Factory returning a nested function."""


def make_counter():
    def counter():
        return 1

    return counter
