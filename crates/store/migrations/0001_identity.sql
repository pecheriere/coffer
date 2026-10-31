CREATE TABLE members (
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('admin', 'member')),
    created_at   INTEGER NOT NULL,
    disabled_at  INTEGER
) STRICT;

CREATE TABLE passkeys (
    id              TEXT PRIMARY KEY,
    member_id       TEXT NOT NULL REFERENCES members (id),
    label           TEXT NOT NULL,
    credential_id   BLOB NOT NULL UNIQUE,
    credential_json TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER
) STRICT;

CREATE INDEX passkeys_member_id ON passkeys (member_id);

CREATE TABLE invites (
    id               TEXT PRIMARY KEY,
    token_hash       BLOB NOT NULL UNIQUE,
    kind             TEXT NOT NULL CHECK (kind IN ('bootstrap', 'new_member', 'reenroll')),
    role             TEXT,
    display_name     TEXT,
    target_member_id TEXT REFERENCES members (id),
    created_by       TEXT REFERENCES members (id),
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    used_at          INTEGER,
    used_by          TEXT REFERENCES members (id),
    revoked_at       INTEGER
) STRICT;

CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY,
    token_hash          BLOB NOT NULL UNIQUE,
    member_id           TEXT NOT NULL REFERENCES members (id),
    created_at          INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    idle_expires_at     INTEGER NOT NULL,
    absolute_expires_at INTEGER NOT NULL
) STRICT;

CREATE INDEX sessions_member_id ON sessions (member_id);

CREATE TABLE agent_tokens (
    id           TEXT PRIMARY KEY,
    member_id    TEXT NOT NULL REFERENCES members (id),
    name         TEXT NOT NULL,
    scope        TEXT NOT NULL CHECK (scope IN ('read', 'write', 'admin')),
    token_hash   BLOB NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    last_used_at INTEGER,
    revoked_at   INTEGER
) STRICT;

CREATE INDEX agent_tokens_member_id ON agent_tokens (member_id);

CREATE TABLE audit_events (
    id                  TEXT PRIMARY KEY,
    at                  INTEGER NOT NULL,
    actor_kind          TEXT NOT NULL CHECK (actor_kind IN ('member', 'system')),
    actor_member_id     TEXT,
    actor_agent_id      TEXT,
    actor_system_reason TEXT,
    action              TEXT NOT NULL,
    entity_type         TEXT NOT NULL,
    entity_id           TEXT NOT NULL,
    changes_json        TEXT NOT NULL,
    CHECK (
        (actor_kind = 'member'
            AND actor_member_id IS NOT NULL
            AND actor_system_reason IS NULL)
        OR (actor_kind = 'system'
            AND actor_system_reason IS NOT NULL
            AND actor_member_id IS NULL
            AND actor_agent_id IS NULL)
    )
) STRICT;

CREATE INDEX audit_events_entity ON audit_events (entity_type, entity_id);
CREATE INDEX audit_events_actor_member_id ON audit_events (actor_member_id);
CREATE INDEX audit_events_actor_agent_id ON audit_events (actor_agent_id);
CREATE INDEX audit_events_at ON audit_events (at);

CREATE TRIGGER audit_events_no_update
BEFORE UPDATE ON audit_events
BEGIN
    SELECT RAISE(ABORT, 'audit_events is append-only');
END;

CREATE TRIGGER audit_events_no_delete
BEFORE DELETE ON audit_events
BEGIN
    SELECT RAISE(ABORT, 'audit_events is append-only');
END;
