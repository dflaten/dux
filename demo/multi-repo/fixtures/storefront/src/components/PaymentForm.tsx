import { useState } from "react";
import { payOrder } from "../lib/api";

export function PaymentForm({ amount }: { amount: number }) {
  const [card, setCard] = useState("");
  const [submitting, setSubmitting] = useState(false);

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    setSubmitting(true);
    try {
      await payOrder({ amount, card });
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="payment-form">
      <label htmlFor="card">Card number</label>
      <input
        id="card"
        value={card}
        onChange={(event) => setCard(event.target.value)}
        autoComplete="cc-number"
      />
      <button type="submit" disabled={submitting}>
        Pay ${amount.toFixed(2)}
      </button>
    </form>
  );
}
