from decimal import Decimal

from app.ledger.entries import LedgerEntry, LedgerWriter


def test_writer_rejects_unbalanced_entries():
    writer = LedgerWriter.in_memory()
    try:
        writer.write_once("bad", [LedgerEntry("cash", Decimal("5.00"), "USD")])
    except ValueError:
        return
    raise AssertionError("expected an unbalanced ledger entry to fail")
