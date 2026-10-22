"""Title search over the in-memory catalogue, with the filter tags of the query applied."""

from __future__ import annotations

from typing import Iterable

from src.catalog.models import Book
from src.catalog.query_tags import SearchFilters, split_query
from src.text.folding import fold


def _within_filters(book: Book, filters: SearchFilters) -> bool:
    if filters.language and book.language != filters.language:
        return False
    if filters.year_from is None:
        return True
    return any(filters.year_from <= e.published.year <= filters.year_to for e in book.editions)


def search_titles(books: Iterable[Book], query: str, *, limit: int = 20) -> list[Book]:
    """Books whose title holds every word of *query*, ignoring case and accents."""
    text, filters = split_query(query)
    words = fold(text).split()
    hits: list[Book] = []
    for book in books:
        title = fold(book.title)
        if all(w in title for w in words) and _within_filters(book, filters):
            hits.append(book)
            if len(hits) == limit:
                break
    return hits
