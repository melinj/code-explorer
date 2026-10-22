"""Filter tags typed into the catalogue search box, such as ``lang:fi`` or ``year:1990-1999``.

Tags are removed from the query before the title search sees it. A tag that cannot be read is
dropped with a warning rather than searched for as text.
"""

import logging
import re
from dataclasses import dataclass

logger = logging.getLogger(__name__)

_LANG = re.compile(r"(?i)\blang:([a-z]{2})\b")
_YEAR = re.compile(r"\byear:(\d{4})(?:-(\d{4}))?(?!\S)")
_ANY_YEAR = re.compile(r"\byear:\S*")


@dataclass
class SearchFilters:
    """What the tags in one query asked for."""

    language: str | None = None
    year_from: int | None = None
    year_to: int | None = None


def split_query(query: str) -> tuple[str, SearchFilters]:
    """The free text of *query* and the filters its tags ask for."""
    filters = SearchFilters()
    languages = _LANG.findall(query)
    if languages:
        filters.language = languages[-1].lower()
        if len(languages) > 1:
            logger.warning(f"several lang: tags in one query; searching {filters.language}")
    years = list(_YEAR.finditer(query))
    if years:
        start, end = years[-1].group(1), years[-1].group(2)
        filters.year_from = int(start)
        filters.year_to = int(end) if end else filters.year_from
        if filters.year_to < filters.year_from:
            logger.warning(f"year range runs backwards, swapping it: {start}-{end}")
            filters.year_from, filters.year_to = filters.year_to, filters.year_from
    else:
        unreadable = list(_ANY_YEAR.finditer(query))
        if unreadable:
            logger.warning(f"ignoring a year tag that is not a year: {unreadable[-1].group(0)}")
    text = _ANY_YEAR.sub("", _LANG.sub("", query))
    return " ".join(text.split()), filters
