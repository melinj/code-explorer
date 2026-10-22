"""Catalogue records: a book, its editions and how each edition is bound."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date
from enum import Enum

# Labels suppliers print on delivery notes, mapped to our own binding names.
_SUPPLIER_LABELS = {
    "hb": "hardcover",
    "hardback": "hardcover",
    "pb": "paperback",
    "softcover": "paperback",
    "epub": "ebook",
}


class Binding(str, Enum):
    """How an edition is bound; the value is what the storefront shows."""

    HARDCOVER = "hardcover"
    PAPERBACK = "paperback"
    EBOOK = "ebook"

    @classmethod
    def parse(cls, label: str) -> Binding:
        """Read a binding from our own name or a supplier's label, ignoring case."""
        key = label.strip().lower()
        try:
            return cls(_SUPPLIER_LABELS.get(key, key))
        except ValueError:
            raise ValueError(f"unrecognised binding label: {label!r}") from None


@dataclass(frozen=True)
class Edition:
    """One purchasable edition of a book."""

    isbn13: str
    binding: Binding
    published: date
    price_cents: int


@dataclass
class Book:
    """A work in the catalogue with every edition we stock."""

    title: str
    authors: list[str]
    language: str = "en"
    editions: list[Edition] = field(default_factory=list)

    def latest(self) -> Edition | None:
        """The most recently published edition, if any."""
        return max(self.editions, key=lambda e: e.published, default=None)

    def in_binding(self, binding: Binding) -> list[Edition]:
        """The editions bound as *binding*, oldest first."""
        return sorted((e for e in self.editions if e.binding is binding), key=lambda e: e.published)

    @property
    def byline(self) -> str:
        """Authors joined for display: "A", "A and B", "A, B and C"."""
        if len(self.authors) < 2:
            return "".join(self.authors)
        return ", ".join(self.authors[:-1]) + " and " + self.authors[-1]
