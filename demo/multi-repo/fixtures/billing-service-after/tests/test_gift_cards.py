from decimal import Decimal

from app.ledger.entries import LedgerWriter
from app.ledger.gift_cards import GiftCardLedger, GiftCardRedemption


def test_redemption_writes_a_balanced_pair():
    writer = LedgerWriter.in_memory()
    GiftCardLedger(writer).record(
        GiftCardRedemption("checkout-42", "GC-DEMO", Decimal("10.00"), "USD")
    )
    entries = writer.entries_for("giftcard:checkout-42")
    assert [entry.amount for entry in entries] == [Decimal("-10.00"), Decimal("10.00")]


def test_duplicate_redemption_is_idempotent():
    writer = LedgerWriter.in_memory()
    ledger = GiftCardLedger(writer)
    redemption = GiftCardRedemption("checkout-42", "GC-DEMO", Decimal("10.00"), "USD")
    ledger.record(redemption)
    ledger.record(redemption)
    assert len(writer.entries_for("giftcard:checkout-42")) == 2
