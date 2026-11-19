import { useState } from "react";
import { GiftCardField } from "./GiftCardField";
import { applyGiftCard, payOrder } from "../lib/api";

export function PaymentForm({ amount }: { amount: number }) {
  const [card, setCard] = useState("");
  const [credit, setCredit] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const total = Math.max(amount - credit, 0);

  async function onApplyGiftCard(code: string) {
    const result = await applyGiftCard({ code, amount: total });
    setCredit((current) => current + result.appliedCents / 100);
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    setSubmitting(true);
    try {
      await payOrder({ amount: total, card });
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="payment-form">
      <GiftCardField onApply={onApplyGiftCard} />
      <label htmlFor="card">Card number</label>
      <input
        id="card"
        value={card}
        onChange={(event) => setCard(event.target.value)}
        autoComplete="cc-number"
      />
      <button type="submit" disabled={submitting}>
        Pay ${total.toFixed(2)}
      </button>
    </form>
  );
}
