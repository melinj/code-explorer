"""VAT rates. Printed books and e-books have a reduced rate in the countries we ship to."""

from decimal import Decimal

_STANDARD = {"FI": Decimal("0.255"), "SE": Decimal("0.25"), "DE": Decimal("0.19"), "EE": Decimal("0.24")}
_BOOKS = {"FI": Decimal("0.14"), "SE": Decimal("0.06"), "DE": Decimal("0.07"), "EE": Decimal("0.09")}


class UnknownCountry(LookupError):
    """We do not ship to, or have no VAT table for, this country."""


def rate_for(country: str, *, book: bool = True) -> Decimal:
    """The VAT rate for a sale into *country*; *book* selects the reduced rate for books."""
    table = _BOOKS if book else _STANDARD
    try:
        return table[country.upper()]
    except KeyError:
        raise UnknownCountry(country) from None
