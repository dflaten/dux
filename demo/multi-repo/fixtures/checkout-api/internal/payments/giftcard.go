package payments

import (
	"errors"
	"time"
)

var ErrNotImplemented = errors.New("gift card apply is not implemented")

type GiftCard struct {
	Code      string
	Balance   int64
	Currency  string
	ExpiresAt time.Time
}

type GiftCardStore interface {
	Lookup(code string) (GiftCard, error)
	Debit(code string, amount int64, idempotencyKey string) (int64, error)
}

func ApplyGiftCard(store GiftCardStore, code string, amountCents int64, idempotencyKey string) (int64, error) {
	return 0, ErrNotImplemented
}
