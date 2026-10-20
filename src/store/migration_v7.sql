-- Recycle-bin state is a trusted record field, projected beside the other display
-- fields so the console can list it without re-reading every record.
ALTER TABLE photo_bindings ADD COLUMN trashed INTEGER NOT NULL DEFAULT 0 CHECK(trashed IN (0,1));
CREATE INDEX photo_bindings_trashed ON photo_bindings(namespace_id,zone_name,trashed,asset_date_ms DESC,photo_id DESC);
UPDATE server_meta SET schema_version=7 WHERE singleton=1;
PRAGMA user_version=7;
