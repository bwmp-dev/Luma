-- The terminal autocomplete overlay is removed, and with it the only reader and
-- writer of this table. It was device-local (no vault_id, no tombstones), so
-- dropping it converges nothing and leaks nothing: the rows are user command
-- text that no longer has a purpose, and keeping them would retain that data
-- for a feature that no longer exists.
DROP INDEX IF EXISTS idx_command_history_scope_rank;
DROP INDEX IF EXISTS idx_command_history_scope_command;
DROP TABLE IF EXISTS command_history;
