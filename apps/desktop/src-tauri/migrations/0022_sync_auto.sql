-- Automatic sync cadence. Cadence is a property of *this* device -- a laptop
-- that is awake all day and a phone on cellular want different answers -- so it
-- lives beside the provider in `sync_state` and never enters the synced bundle.
--
-- Placing it in columns rather than in the `state` JSON is deliberate:
-- `sync_configure` rewrites `state` wholesale when the provider changes, and the
-- cadence the user chose must survive that. Disabling sync drops the row, which
-- is the one place these settings are meant to disappear.
--
-- Existing configurations adopt the same defaults a new one gets, so a user who
-- set sync up before this migration stops having to remember to press "Sync
-- now". Nothing runs automatically unless the vault's key is already available
-- on this device, so a vault whose passphrase is not remembered still waits.
ALTER TABLE sync_state ADD COLUMN auto_push_mode TEXT NOT NULL DEFAULT 'on-change';
ALTER TABLE sync_state ADD COLUMN auto_push_interval_minutes INTEGER NOT NULL DEFAULT 15;
ALTER TABLE sync_state ADD COLUMN auto_pull_interval_minutes INTEGER NOT NULL DEFAULT 15;
ALTER TABLE sync_state ADD COLUMN auto_pull_on_start INTEGER NOT NULL DEFAULT 1;
ALTER TABLE sync_state ADD COLUMN auto_pull_on_focus INTEGER NOT NULL DEFAULT 1;
