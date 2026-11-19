from dataclasses import dataclass
from decimal import Decimal
from typing import Dict, Iterable, List


@dataclass(frozen=True)
class LedgerEntry:
    account: str
    amount: Decimal
    currency: str


class LedgerWriter:
    """Writes balanced journal entries, once per idempotency key."""

    def __init__(self) -> None:
        self._entries: Dict[str, List[LedgerEntry]] = {}

    @classmethod
    def in_memory(cls) -> "LedgerWriter":
        return cls()

    def write_once(self, key: str, entries: Iterable[LedgerEntry]) -> None:
        entries = list(entries)
        debits = sum(entry.amount for entry in entries if entry.amount < 0)
        credits = sum(entry.amount for entry in entries if entry.amount > 0)
        if debits + credits != 0:
            raise ValueError("ledger entry is not balanced")
        self._entries.setdefault(key, entries)

    def entries_for(self, key: str) -> List[LedgerEntry]:
        return list(self._entries.get(key, []))
