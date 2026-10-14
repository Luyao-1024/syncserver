-- Unknown historical edit times remain unknown, never backfilled with migration time.
ALTER TABLE records ADD COLUMN events_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(events_json));
ALTER TABLE record_versions ADD COLUMN events_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(events_json));
UPDATE server_meta SET schema_version=3 WHERE singleton=1;
PRAGMA user_version=3;
