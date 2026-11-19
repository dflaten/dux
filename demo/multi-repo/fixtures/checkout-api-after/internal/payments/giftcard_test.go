package payments

import (
	"context"
	"testing"
	"time"
)

func TestApplyGiftCardPartialRedemption(t *testing.T) {
	store := fakeStore{card: GiftCard{Balance: 2500, Currency: "usd", ExpiresAt: time.Now().Add(time.Hour)}}
	result, err := ApplyGiftCard(context.Background(), store, "GC-DEMO", 1000, "checkout-42")
	if err != nil {
		t.Fatal(err)
	}
	if result.AppliedCents != 1000 || result.RemainingCents != 1500 {
		t.Fatalf("unexpected result: %+v", result)
	}
}

func TestApplyGiftCardRejectsExpired(t *testing.T) {
	store := fakeStore{card: GiftCard{Balance: 2500, ExpiresAt: time.Now().Add(-time.Hour)}}
	if _, err := ApplyGiftCard(context.Background(), store, "GC-DEMO", 1000, "checkout-43"); err != ErrCardExpired {
		t.Fatalf("expected ErrCardExpired, got %v", err)
	}
}

type fakeStore struct {
	card GiftCard
}

func (f fakeStore) Lookup(ctx context.Context, code string) (GiftCard, error) {
	return f.card, nil
}

func (f fakeStore) Debit(ctx context.Context, code string, amount int64, idempotencyKey string) (int64, error) {
	return f.card.Balance - amount, nil
}
