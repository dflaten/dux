from dataclasses import dataclass
from decimal import Decimal

from app.ledger.entries import LedgerEntry, LedgerWriter


@dataclass(frozen=True)
class GiftCardRedemption:
    checkout_id: str
    card_code: str
    amount: Decimal
    currency: str


class GiftCardLedger:
    def __init__(self, writer: LedgerWriter) -> None:
        self.writer = writer

    def record(self, redemption: GiftCardRedemption) -> None:
        key = f"giftcard:{redemption.checkout_id}"
        self.writer.write_once(
            key,
            [
                LedgerEntry("gift_card_liability", -redemption.amount, redemption.currency),
                LedgerEntry("deferred_revenue", redemption.amount, redemption.currency),
            ],
        )
