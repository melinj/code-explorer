"""ISBN normalisation: strip separators, check digits, convert ISBN-10 to ISBN-13."""

import re

_SEPARATORS = re.compile(r"[\s\-]+")


def normalise(raw: str) -> str:
    """*raw* without spaces or hyphens, with a lower-case ``x`` check digit upper-cased."""
    return _SEPARATORS.sub("", raw).upper()


def isbn13_check_digit(first12: str) -> str:
    """The check digit completing the twelve digits *first12*."""
    total = sum(int(d) * (3 if i % 2 else 1) for i, d in enumerate(first12))
    return str((10 - total % 10) % 10)


def isbn10_is_valid(digits: str) -> bool:
    """Whether ten characters form a valid ISBN-10 (the last may be ``X``, meaning 10)."""
    if len(digits) != 10 or not digits[:9].isdigit() or digits[9] not in "0123456789X":
        return False
    last = 10 if digits[9] == "X" else int(digits[9])
    weighted = sum((10 - i) * int(d) for i, d in enumerate(digits[:9]))
    return (weighted + last) % 11 == 0


def to_isbn13(raw: str) -> str:
    """The ISBN-13 for *raw*, an ISBN-10 or ISBN-13 in any common spelling."""
    digits = normalise(raw)
    if len(digits) == 13 and digits.isdigit():
        if isbn13_check_digit(digits[:12]) != digits[12]:
            raise ValueError(f"bad ISBN-13 check digit in {raw}")
        return digits
    if not isbn10_is_valid(digits):
        raise ValueError(f"not an ISBN: {raw}")
    body = "978" + digits[:9]
    return body + isbn13_check_digit(body)
