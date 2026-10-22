"""Who is making a request: the verified customer and how they signed in."""

from datetime import datetime, timezone
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from src.access.roles import Role


class Customer(BaseModel):
    """A verified caller, built by the session middleware and never from request input."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    customer_id: str = Field(pattern=r"^c_[0-9a-f]{12}$")
    display_name: str = Field(min_length=1, max_length=80)
    roles: frozenset[Role] = Field(default_factory=frozenset)
    signed_in_with: Literal["password", "magic_link", "api_key"] = "password"
    signed_in_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    def is_staff(self) -> bool:
        """Whether any back-office role was granted (customers hold none)."""
        return bool(self.roles)

    def may(self, needed: Role) -> bool:
        """Whether one of this caller's roles is *needed* or outranks it."""
        return any(role is needed or role.outranks(needed) for role in self.roles)
