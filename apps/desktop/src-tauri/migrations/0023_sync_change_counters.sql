-- Monotonic per-vault change sequences for automatic sync. Row timestamps have
-- one-second resolution, so they cannot reliably identify rapid rewrites.
CREATE TABLE sync_change_counters (
    vault_id TEXT PRIMARY KEY,
    version  INTEGER NOT NULL DEFAULT 0
);

INSERT INTO sync_change_counters (vault_id, version)
SELECT id, 0 FROM vaults;

CREATE TRIGGER sync_change_vault_insert AFTER INSERT ON vaults BEGIN
    INSERT OR IGNORE INTO sync_change_counters (vault_id, version) VALUES (NEW.id, 0);
END;
CREATE TRIGGER sync_change_vault_update AFTER UPDATE ON vaults BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_vault_delete AFTER DELETE ON vaults BEGIN
    DELETE FROM sync_change_counters WHERE vault_id = OLD.id;
END;

CREATE TRIGGER sync_change_hosts_insert AFTER INSERT ON hosts
WHEN NEW.is_ephemeral = 0 BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_hosts_update AFTER UPDATE ON hosts BEGIN
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT NEW.vault_id, 1 WHERE NEW.is_ephemeral = 0
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1
    WHERE OLD.is_ephemeral = 0 AND (NEW.is_ephemeral <> 0 OR OLD.vault_id <> NEW.vault_id)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_hosts_delete AFTER DELETE ON hosts
WHEN OLD.is_ephemeral = 0 BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_host_groups_insert AFTER INSERT ON host_groups BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_host_groups_update AFTER UPDATE ON host_groups BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1 WHERE OLD.vault_id <> NEW.vault_id
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_host_groups_delete AFTER DELETE ON host_groups BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_keys_insert AFTER INSERT ON key_references BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_keys_update AFTER UPDATE ON key_references BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1 WHERE OLD.vault_id <> NEW.vault_id
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_keys_delete AFTER DELETE ON key_references BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_identities_insert AFTER INSERT ON identities BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_identities_update AFTER UPDATE ON identities BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1 WHERE OLD.vault_id <> NEW.vault_id
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_identities_delete AFTER DELETE ON identities BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_snippets_insert AFTER INSERT ON snippets BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_snippets_update AFTER UPDATE ON snippets BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1 WHERE OLD.vault_id <> NEW.vault_id
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_snippets_delete AFTER DELETE ON snippets BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_tombstones_insert AFTER INSERT ON tombstones BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_tombstones_update AFTER UPDATE ON tombstones BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (NEW.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
    INSERT INTO sync_change_counters (vault_id, version)
    SELECT OLD.vault_id, 1 WHERE OLD.vault_id <> NEW.vault_id
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_tombstones_delete AFTER DELETE ON tombstones BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES (OLD.vault_id, 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_profiles_insert AFTER INSERT ON terminal_profiles BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_profiles_update AFTER UPDATE ON terminal_profiles BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_profiles_delete AFTER DELETE ON terminal_profiles BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;

CREATE TRIGGER sync_change_settings_insert AFTER INSERT ON settings
WHEN NEW.key <> 'workspace.snapshot' BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_settings_update AFTER UPDATE ON settings
WHEN OLD.key <> 'workspace.snapshot' OR NEW.key <> 'workspace.snapshot' BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
CREATE TRIGGER sync_change_settings_delete AFTER DELETE ON settings
WHEN OLD.key <> 'workspace.snapshot' BEGIN
    INSERT INTO sync_change_counters (vault_id, version) VALUES ('personal', 1)
    ON CONFLICT(vault_id) DO UPDATE SET version = version + 1;
END;
