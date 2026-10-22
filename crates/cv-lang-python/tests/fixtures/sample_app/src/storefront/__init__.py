"""The shop's HTTP handlers. Payment providers are plugins, loaded by name at startup."""

from __future__ import annotations

import importlib
from typing import Iterable, Protocol


class PaymentProvider(Protocol):
    def charge(self, customer_id: str, amount_minor: int, currency: str) -> str: ...


def load_providers(names: Iterable[str]) -> dict[str, PaymentProvider]:
    """Import ``src.storefront.payments.<name>`` for each name and call its ``provider()``."""
    providers: dict[str, PaymentProvider] = {}
    for name in names:
        module = importlib.import_module(f"src.storefront.payments.{name}")
        providers[name] = module.provider()
    return providers
