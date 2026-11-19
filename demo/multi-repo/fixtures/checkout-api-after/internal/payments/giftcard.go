package payments

import (
	"context"
	"errors"
	"time"
)

var (
	ErrCardExpired = errors.New("gift card expired")
	ErrCardDrained = errors.New("gift card has no remaining balance")
)

type GiftCard struct {
	Code      string
	Balance   int64
	Currency  string
	ExpiresAt time.Time
}

type GiftCardStore interface {
	Lookup(ctx context.Context, code string) (GiftCard, error)
	Debit(ctx context.Context, code string, amount int64, idempotencyKey string) (int64, error)
}

type ApplyResult struct {
	AppliedCents   int64  `json:"applied_cents"`
	RemainingCents int64  `json:"remaining_cents"`
	Currency       string `json:"currency"`
	ReceiptLine    string `json:"receipt_line"`
}

func ApplyGiftCard(ctx context.Context, store GiftCardStore, code string, amountCents int64, idempotencyKey string) (ApplyResult, error) {
	card, err := store.Lookup(ctx, code)
	if err != nil {
		return ApplyResult{}, err
	}
	if time.Now().After(card.ExpiresAt) {
		return ApplyResult{}, ErrCardExpired
	}
	if card.Balance <= 0 {
		return ApplyResult{}, ErrCardDrained
	}
	applied := min(amountCents, card.Balance)
	remaining, err := store.Debit(ctx, code, applied, idempotencyKey)
	if err != nil {
		return ApplyResult{}, err
	}
	return ApplyResult{
		AppliedCents:   applied,
		RemainingCents: remaining,
		Currency:       card.Currency,
		ReceiptLine:    "Gift card " + code,
	}, nil
}
