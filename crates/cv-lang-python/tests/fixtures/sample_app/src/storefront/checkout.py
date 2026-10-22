"""POST /checkout: turn the signed-in customer's basket into an invoice."""

from __future__ import annotations

from typing import Any

from src.access.session import current_customer
from src.access.throttle import allow
from src.billing import build_invoice
from src.storefront.errors import CheckoutRejected


def checkout(basket: Any, country: str, rules: list) -> dict[str, str]:
    """Invoice *basket* for the current customer; returns the invoice number and total."""
    customer = current_customer()
    if not allow(f"checkout:{customer.customer_id}", rate=0.05, burst=3):
        raise CheckoutRejected("too_many_checkouts", "wait a minute before trying again")
    if not basket.lines:
        raise CheckoutRejected("empty_basket", f"basket {basket.id} has nothing in it")
    invoice = build_invoice([(line.edition, line.quantity) for line in basket.lines], country, rules)
    return {"invoice": invoice.number, "total": str(invoice.total)}
