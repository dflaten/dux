import { CheckoutSummary } from "../components/CheckoutSummary";
import { PaymentForm } from "../components/PaymentForm";
import { useCart } from "../lib/cart";

export default function CheckoutPage() {
  const { items, subtotal } = useCart();

  return (
    <main className="checkout-shell">
      <CheckoutSummary items={items} subtotal={subtotal} />
      <PaymentForm amount={subtotal} />
    </main>
  );
}
