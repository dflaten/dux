import { useState } from "react";

export function GiftCardField({
  onApply,
}: {
  onApply: (code: string) => Promise<void>;
}) {
  const [code, setCode] = useState("");
  const [applied, setApplied] = useState(false);

  async function apply() {
    if (!code.trim()) return;
    await onApply(code.trim().toUpperCase());
    setApplied(true);
  }

  return (
    <div className="gift-card-field">
      <label htmlFor="gift-card">Gift card</label>
      <input
        id="gift-card"
        value={code}
        onChange={(event) => setCode(event.target.value)}
        placeholder="GC-XXXX-XXXX"
        disabled={applied}
      />
      <button type="button" onClick={apply} disabled={applied}>
        {applied ? "Applied" : "Apply"}
      </button>
    </div>
  );
}
