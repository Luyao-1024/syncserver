-- Read projection for the web console's photo index.
--
-- `photo_bindings` already is the server's trusted photo graph: it is written in
-- the same transaction as the records, versions and changes that produce it. These
-- columns denormalise the display fields the gallery needs so that listing by
-- capture time, filtering by media kind or grouping by album are index operations
-- instead of repeated json_extract calls over every record.
ALTER TABLE photo_bindings ADD COLUMN asset_date_ms INTEGER;
ALTER TABLE photo_bindings ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1));
ALTER TABLE photo_bindings ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0,1));
ALTER TABLE photo_bindings ADD COLUMN album TEXT NOT NULL DEFAULT '';
ALTER TABLE photo_bindings ADD COLUMN media_kind INTEGER NOT NULL DEFAULT 2 CHECK(media_kind IN (0,1,2));
CREATE INDEX photo_bindings_dated ON photo_bindings(namespace_id,zone_name,asset_date_ms DESC,photo_id DESC);
CREATE INDEX photo_bindings_kind ON photo_bindings(namespace_id,zone_name,media_kind,asset_date_ms DESC,photo_id DESC);
CREATE INDEX photo_bindings_album ON photo_bindings(namespace_id,zone_name,album,asset_date_ms DESC,photo_id DESC);
UPDATE server_meta SET schema_version=6 WHERE singleton=1;
PRAGMA user_version=6;
