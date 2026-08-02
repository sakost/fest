"""Nested classes — exercises descend_class recursion."""


class Outer:
    class Inner:
        VALUE = 42

        def compute(self) -> int:
            return self.VALUE * 2
