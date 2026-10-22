"""Back-office roles, from least to most trusted."""

from enum import StrEnum
from typing import Iterable


class Role(StrEnum):
    """A staff role; each member may do everything the members before it may."""

    CLERK = "clerk"
    BUYER = "buyer"
    MANAGER = "manager"
    OWNER = "owner"

    @property
    def rank(self) -> int:
        return list(Role).index(self)

    def outranks(self, other: "Role") -> bool:
        return self.rank > other.rank

    @classmethod
    def parse_list(cls, header: str) -> frozenset["Role"]:
        """The roles named in a comma-separated header value; unknown names are skipped."""
        known = {role.value for role in cls}
        names = (part.strip().lower() for part in header.split(","))
        return frozenset(cls(name) for name in names if name in known)


def highest(roles: Iterable[Role]) -> Role | None:
    """The most trusted of *roles*, or None when there are none."""
    return max(roles, key=lambda role: role.rank, default=None)
