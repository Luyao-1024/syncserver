CREATE TABLE records (
    namespace_id INTEGER NOT NULL,
    zone_name TEXT NOT NULL,
    record_name TEXT NOT NULL,
    record_type TEXT NOT NULL,
    revision TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq > 0),
    fields_json TEXT NOT NULL CHECK (json_valid(fields_json)),
    deleted INTEGER NOT NULL CHECK (deleted IN (0,1)),
    PRIMARY KEY (namespace_id, zone_name, record_name),
    FOREIGN KEY (namespace_id, zone_name) REFERENCES zones(namespace_id, zone_name),
    FOREIGN KEY (namespace_id, zone_name, record_name, seq) REFERENCES record_versions(namespace_id, zone_name, record_name, seq) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE INDEX records_type ON records(namespace_id, zone_name, record_type, record_name) WHERE deleted=0;
CREATE TABLE record_versions (
    namespace_id INTEGER NOT NULL,
    zone_name TEXT NOT NULL,
    record_name TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq > 0),
    record_type TEXT NOT NULL,
    revision TEXT NOT NULL,
    fields_json TEXT NOT NULL CHECK (json_valid(fields_json)),
    deleted INTEGER NOT NULL CHECK (deleted IN (0,1)),
    PRIMARY KEY (namespace_id, seq),
    UNIQUE (namespace_id, zone_name, record_name, seq),
    FOREIGN KEY (namespace_id, zone_name, record_name) REFERENCES records(namespace_id, zone_name, record_name)
) STRICT;
CREATE INDEX record_versions_snapshot ON record_versions(namespace_id, zone_name, record_name, seq DESC);
CREATE TABLE changes (
    namespace_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    zone_name TEXT NOT NULL,
    record_name TEXT NOT NULL,
    PRIMARY KEY (namespace_id, seq),
    FOREIGN KEY (namespace_id, zone_name, record_name, seq) REFERENCES record_versions(namespace_id, zone_name, record_name, seq)
) STRICT;
CREATE INDEX changes_zone ON changes(namespace_id, zone_name, seq);
CREATE TABLE photo_bindings (
    namespace_id INTEGER NOT NULL,
    zone_name TEXT NOT NULL,
    photo_id TEXT NOT NULL,
    master_record_name TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    content_generation TEXT NOT NULL,
    original_asset_id TEXT NOT NULL,
    thumbnail_asset_id TEXT NOT NULL,
    PRIMARY KEY (namespace_id, zone_name, photo_id),
    UNIQUE (namespace_id, zone_name, master_record_name),
    UNIQUE (namespace_id, zone_name, relative_path),
    FOREIGN KEY (namespace_id, zone_name, photo_id) REFERENCES records(namespace_id, zone_name, record_name),
    FOREIGN KEY (namespace_id, zone_name, master_record_name) REFERENCES records(namespace_id, zone_name, record_name)
) STRICT;
CREATE TABLE operation_receipts (
    namespace_id INTEGER NOT NULL REFERENCES namespaces(namespace_id),
    operation_id TEXT NOT NULL,
    item_index INTEGER NOT NULL CHECK (item_index >= -1),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint)=32),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    PRIMARY KEY (namespace_id, operation_id, item_index)
) STRICT;
UPDATE server_meta SET schema_version=2 WHERE singleton=1;
PRAGMA user_version=2;
