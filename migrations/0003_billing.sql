-- The optional stripe feature owns these tables. No rows are created when disabled.
CREATE TABLE billing_accounts (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    customer_id text UNIQUE,
    paid_at timestamptz
);
CREATE TABLE billing_checkouts (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES billing_accounts(user_id) ON DELETE CASCADE,
    mode text NOT NULL CHECK (mode IN ('payment', 'subscription')),
    price_id text NOT NULL,
    session_id text UNIQUE,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX billing_checkouts_active ON billing_checkouts(user_id) WHERE active;
CREATE TABLE billing_subscriptions (
    id text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES billing_accounts(user_id) ON DELETE CASCADE,
    status text NOT NULL,
    access_active boolean NOT NULL DEFAULT false,
    cancel_at_period_end boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX billing_subscriptions_user ON billing_subscriptions(user_id);
