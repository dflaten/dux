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
