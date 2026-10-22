"""Read editions from a supplier's ONIX 3.0 product feed (only the fields we stock from)."""

from __future__ import annotations

import xml.etree.ElementTree as ET
from datetime import date
from decimal import Decimal
from typing import Iterator

from .isbn import to_isbn13
from .models import Binding, Edition

_NS = {"onix": "http://ns.editeur.org/onix/3.0/reference"}

# ONIX product form codes we sell, by binding.
_FORMS = {"BB": Binding.HARDCOVER, "BC": Binding.PAPERBACK, "ED": Binding.EBOOK}


def _text(product: ET.Element, path: str) -> str | None:
    element = product.find(path, _NS)
    return element.text.strip() if element is not None and element.text else None


def read_editions(feed: bytes) -> Iterator[Edition]:
    """The editions in *feed*, skipping products in a form we do not sell."""
    root = ET.fromstring(feed)
    for product in root.iterfind("onix:Product", _NS):
        form = _FORMS.get(_text(product, "onix:DescriptiveDetail/onix:ProductForm") or "")
        isbn = _text(product, "onix:ProductIdentifier[onix:ProductIDType='15']/onix:IDValue")
        if form is None or isbn is None:
            continue
        published = _text(product, "onix:PublishingDetail/onix:PublishingDate/onix:Date") or "19700101"
        price = _text(product, "onix:ProductSupply/onix:SupplyDetail/onix:Price/onix:PriceAmount") or "0"
        yield Edition(
            isbn13=to_isbn13(isbn),
            binding=form,
            published=date(int(published[:4]), int(published[4:6]), int(published[6:8])),
            price_cents=int(Decimal(price) * 100),
        )
