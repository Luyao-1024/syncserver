CREATE TABLE assets(
 namespace_id INTEGER NOT NULL REFERENCES namespaces(namespace_id), asset_id TEXT NOT NULL,
 sha256 TEXT NOT NULL CHECK(length(sha256)=64), size INTEGER NOT NULL CHECK(size>0),
 mime_type TEXT NOT NULL,width INTEGER,height INTEGER, thumbnail_validated INTEGER NOT NULL CHECK(thumbnail_validated IN(0,1)),
 PRIMARY KEY(namespace_id,asset_id), UNIQUE(namespace_id,sha256,size)
) STRICT;
CREATE TABLE upload_sessions(
 namespace_id INTEGER NOT NULL REFERENCES namespaces(namespace_id),session_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN('Reserved','Receiving','PublishPrepared','Published','Failed','Expired')),
 zone_name TEXT NOT NULL,record_name TEXT NOT NULL,field_name TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN('original','thumbnail')),
 intent TEXT NOT NULL CHECK(intent IN('create','update')),expected_size INTEGER NOT NULL CHECK(expected_size>0),expected_sha256 TEXT,
 lease_id TEXT NOT NULL,fence INTEGER NOT NULL,expires_at_ms INTEGER NOT NULL,asset_id TEXT,error TEXT,staging_name TEXT,
 PRIMARY KEY(namespace_id,session_id),FOREIGN KEY(namespace_id,zone_name) REFERENCES zones(namespace_id,zone_name),FOREIGN KEY(namespace_id,asset_id) REFERENCES assets(namespace_id,asset_id)
) STRICT;
CREATE INDEX sessions_lease_state ON upload_sessions(namespace_id,lease_id,fence,state);
CREATE INDEX sessions_expiry ON upload_sessions(namespace_id,state,expires_at_ms);
CREATE TABLE publish_intents(
 namespace_id INTEGER NOT NULL,session_id TEXT NOT NULL,staging_name TEXT NOT NULL,asset_json TEXT NOT NULL CHECK(json_valid(asset_json)),
 state TEXT NOT NULL CHECK(state IN('Prepared','Published','Quarantined')),
 PRIMARY KEY(namespace_id,session_id),FOREIGN KEY(namespace_id,session_id) REFERENCES upload_sessions(namespace_id,session_id)
) STRICT;
CREATE TABLE upload_reservations(
 namespace_id INTEGER NOT NULL REFERENCES namespaces(namespace_id),operation_id TEXT NOT NULL,fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),session_ids TEXT NOT NULL CHECK(json_valid(session_ids)),
 PRIMARY KEY(namespace_id,operation_id)
) STRICT;
UPDATE server_meta SET schema_version=5 WHERE singleton=1;
PRAGMA user_version=5;
