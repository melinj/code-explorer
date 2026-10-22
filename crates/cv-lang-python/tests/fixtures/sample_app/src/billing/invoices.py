"""Invoices: priced lines with VAT, posted to the ledger as soon as they are issued."""

from __future__ import annotations

import itertools
from dataclasses import dataclass
from datetime import date
from typing import TYPE_CHECKING

from src.billing import tax
from src.billing.discounts import best_price
from src.ledger.journal import post_entry

from .currency import Money

if TYPE_CHECKING:
    from ..catalog.models import Edition
    from .discounts import DiscountRule

_numbers = itertools.count(1)


@dataclass(frozen=True)
class InvoiceLine:
    isbn13: str
    quantity: int
    net: Money
    vat: Money
    discount_code: str | None = None


@dataclass(frozen=True)
class Invoice:
    number: str
    issued: date
    country: str
    lines: tuple[InvoiceLine, ...]

    @property
    def total(self) -> Money:
        """Net plus VAT over every line."""
        total = Money.zero(self.lines[0].net.currency if self.lines else "EUR")
        for line in self.lines:
            total = total + line.net + line.vat
        return total


def build_invoice(
    items: list[tuple[Edition, int]],
    country: str,
    rules: list[DiscountRule],
    *,
    today: date | None = None,
) -> Invoice:
    """Price each (edition, quantity) pair, apply its best discount, add VAT and post the total."""
    rate = tax.rate_for(country, book=True)
    lines = []
    for edition, quantity in items:
        net, code = best_price(rules, edition.isbn13, Money(edition.price_cents), quantity)
        lines.append(InvoiceLine(edition.isbn13, quantity, net, net.scaled(rate), code))
    issued = today or date.today()
    invoice = Invoice(f"INV-{issued:%Y}-{next(_numbers):05d}", issued, country, tuple(lines))
    post_entry("sales", invoice.number, invoice.total.minor)
    return invoice
