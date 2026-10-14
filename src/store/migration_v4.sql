CREATE TABLE upload_lease (
 namespace_id INTEGER PRIMARY KEY REFERENCES namespaces(namespace_id),
 fence INTEGER NOT NULL CHECK(fence>=0),
 lease_id TEXT, device_id TEXT, zones_json TEXT CHECK(zones_json IS NULL OR json_valid(zones_json)),
 deadline_tick_ms INTEGER, expires_at_ms INTEGER,
 phase TEXT CHECK(phase IS NULL OR phase IN('checking','delete','modify','create')),
 verified_head INTEGER,
 CHECK((lease_id IS NULL AND device_id IS NULL AND zones_json IS NULL AND deadline_tick_ms IS NULL AND expires_at_ms IS NULL AND phase IS NULL AND verified_head IS NULL) OR (lease_id IS NOT NULL AND device_id IS NOT NULL AND zones_json IS NOT NULL AND deadline_tick_ms IS NOT NULL AND expires_at_ms IS NOT NULL AND phase IS NOT NULL))
) STRICT;
INSERT INTO upload_lease(namespace_id,fence) VALUES(1,0);
UPDATE server_meta SET schema_version=4 WHERE singleton=1;
PRAGMA user_version=4;
