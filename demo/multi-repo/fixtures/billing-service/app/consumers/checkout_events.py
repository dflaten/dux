from app.ledger.entries import LedgerWriter


class CheckoutEventConsumer:
    def __init__(self, writer: LedgerWriter) -> None:
        self.writer = writer

    def handle(self, event: dict) -> None:
        match event["type"]:
            case "payment.captured":
                self._record_capture(event)
            case _:
                return

    def _record_capture(self, event: dict) -> None:
        amount = event["amount_cents"]
        self.writer.write_once(
            f"capture:{event['idempotency_key']}",
            [],
        )
