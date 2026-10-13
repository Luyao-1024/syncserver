CREATE TABLE server_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    database_generation TEXT NOT NULL,
    signing_fingerprint BLOB NOT NULL CHECK (length(signing_fingerprint) = 32)
) STRICT;
CREATE TABLE namespaces (
    namespace_id INTEGER PRIMARY KEY CHECK (namespace_id = 1),
    tenant TEXT NOT NULL,
    container TEXT NOT NULL,
    environment TEXT NOT NULL CHECK (environment = 'production'),
    database_name TEXT NOT NULL CHECK (database_name = 'private'),
    UNIQUE (tenant, container, environment, database_name)
) STRICT;
CREATE TABLE zones (
    namespace_id INTEGER NOT NULL REFERENCES namespaces(namespace_id),
    zone_name TEXT NOT NULL,
    owner TEXT NOT NULL CHECK (owner = '_defaultOwner'),
    PRIMARY KEY (namespace_id, zone_name)
) STRICT;
CREATE TABLE namespace_counters (
    namespace_id INTEGER PRIMARY KEY REFERENCES namespaces(namespace_id),
    last_committed_seq INTEGER NOT NULL CHECK (last_committed_seq >= 0)
) STRICT;
CREATE TABLE service_starts (
    run_id TEXT PRIMARY KEY,
    started_at_ms INTEGER NOT NULL,
    recovered_at_ms INTEGER,
    stopped_at_ms INTEGER,
    state TEXT NOT NULL CHECK (state IN ('Recovering', 'Ready', 'Stopped', 'Interrupted'))
) STRICT;
CREATE INDEX service_starts_pending ON service_starts(state) WHERE state IN ('Recovering', 'Ready');
