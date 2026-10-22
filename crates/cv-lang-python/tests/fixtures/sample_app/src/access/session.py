"""Binds the verified customer to the request being handled, for handlers that need it."""

from __future__ import annotations

from contextvars import ContextVar, Token

from starlette.requests import Request

from src.access.identity import Customer
from src.access.roles import Role

STAFF_ROLES_HEADER = "x-staff-roles"

_current: ContextVar[Customer | None] = ContextVar(
    "current_customer", default=None
)


def bind(customer: Customer | None) -> Token[Customer | None]:
    """Make *customer* the current one; pass the token to :func:`unbind` afterwards."""
    return _current.set(customer)


def unbind(token: Token[Customer | None]) -> None:
    _current.reset(token)


def current_customer() -> Customer:
    """The customer of the request being handled."""
    customer = _current.get()
    if customer is None:
        raise LookupError("no customer for this request — is SessionMiddleware installed?")
    return customer


def staff_roles(request: Request) -> frozenset[Role]:
    """The roles the trusted gateway put in the request headers (none for customers)."""
    header = request.headers.get(STAFF_ROLES_HEADER, "")
    return Role.parse_list(header)
