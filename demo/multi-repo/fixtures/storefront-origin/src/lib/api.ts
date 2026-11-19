export async function payOrder(input: { amount: number; card: string }) {
  const response = await fetch("/api/payments", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  });
  if (!response.ok) {
    throw new Error("Payment failed");
  }
  return response.json();
}

export async function applyGiftCard(input: { code: string; amount: number }) {
  const response = await fetch("/v1/gift-cards/apply", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      code: input.code,
      amountCents: Math.round(input.amount * 100),
      idempotencyKey: crypto.randomUUID(),
    }),
  });
  if (!response.ok) {
    throw new Error("Gift card could not be applied");
  }
  return response.json() as Promise<{
    appliedCents: number;
    remainingCents: number;
  }>;
}
