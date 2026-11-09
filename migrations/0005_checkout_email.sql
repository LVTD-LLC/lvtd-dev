-- Preserve Stripe idempotency request bodies across account email changes.
ALTER TABLE billing_checkouts ADD COLUMN email text;
UPDATE billing_checkouts SET email = users.email FROM users
    WHERE billing_checkouts.user_id = users.id;
ALTER TABLE billing_checkouts ALTER COLUMN email SET NOT NULL;
