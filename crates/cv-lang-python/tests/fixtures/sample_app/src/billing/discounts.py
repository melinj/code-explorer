"""Discount rules applied to a basket line before VAT."""

from __future__ import annotations

import abc
from dataclasses import dataclass
from decimal import Decimal

from .currency import Money


class DiscountRule(abc.ABC):
    """A rule that may lower the price of one line."""

    code: str

    @abc.abstractmethod
    def applies_to(self, isbn13: str, quantity: int) -> bool:
        """Whether this rule lowers the price of *quantity* copies of *isbn13*."""

    @abc.abstractmethod
    def discounted(self, unit_price: Money, quantity: int) -> Money:
        """The price of the whole line after the discount."""


@dataclass
class PercentOff(DiscountRule):
    """A percentage off every copy of the listed titles."""

    code: str
    percent: int
    isbns: frozenset[str]

    def applies_to(self, isbn13: str, quantity: int) -> bool:
        return isbn13 in self.isbns and quantity > 0

    def discounted(self, unit_price: Money, quantity: int) -> Money:
        factor = Decimal(100 - self.percent) / 100
        return Money(unit_price.minor * quantity, unit_price.currency).scaled(factor)


@dataclass
class ThreeForTwo(DiscountRule):
    """Every third copy of a title is free."""

    code: str = "3FOR2"

    def applies_to(self, isbn13: str, quantity: int) -> bool:
        return quantity >= 3

    def discounted(self, unit_price: Money, quantity: int) -> Money:
        paid = quantity - quantity // 3
        return Money(unit_price.minor * paid, unit_price.currency)


def best_price(
    rules: list[DiscountRule], isbn13: str, unit_price: Money, quantity: int
) -> tuple[Money, str | None]:
    """The lowest price any applicable rule gives the line, and that rule's code."""
    best: tuple[Money, str | None] = (Money(unit_price.minor * quantity, unit_price.currency), None)
    for rule in rules:
        if rule.applies_to(isbn13, quantity):
            price = rule.discounted(unit_price, quantity)
            if price < best[0]:
                best = (price, rule.code)
    return best
