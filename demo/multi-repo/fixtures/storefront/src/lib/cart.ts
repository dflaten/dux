const demoItems = [
  { sku: "tee-olive", name: "Everyday tee", price: 38 },
  { sku: "cap-canvas", name: "Canvas cap", price: 24 },
];

export function useCart() {
  const items = demoItems;
  return {
    items,
    subtotal: items.reduce((total, item) => total + item.price, 0),
  };
}
