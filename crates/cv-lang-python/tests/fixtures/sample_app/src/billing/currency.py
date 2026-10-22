"""Money as whole minor units of one currency; arithmetic refuses to mix currencies."""

from __future__ import annotations

from dataclasses import dataclass
from decimal import ROUND_HALF_EVEN, Decimal


class CurrencyMismatch(ValueError):
    """Two amounts in different currencies were combined."""


@dataclass(frozen=True, order=True)
class Money:
    """An amount in the smallest unit of *currency* (cents for EUR)."""

    minor: int
    currency: str = "EUR"

    @classmethod
    def zero(cls, currency: str = "EUR") -> Money:
        return cls(0, currency)

    def __add__(self, other: Money) -> Money:
        self._same_currency(other)
        return Money(self.minor + other.minor, self.currency)

    def __sub__(self, other: Money) -> Money:
        self._same_currency(other)
        return Money(self.minor - other.minor, self.currency)

    def scaled(self, factor: Decimal) -> Money:
        """This amount times *factor*, rounded half to even to a whole minor unit."""
        exact = Decimal(self.minor) * factor
        return Money(int(exact.quantize(Decimal(1), rounding=ROUND_HALF_EVEN)), self.currency)

    def _same_currency(self, other: Money) -> None:
        if other.currency != self.currency:
            raise CurrencyMismatch(f"{self.currency} vs {other.currency}")

    def __str__(self) -> str:
        sign = "-" if self.minor < 0 else ""
        whole, cents = divmod(abs(self.minor), 100)
        return f"{sign}{whole}.{cents:02d} {self.currency}"
