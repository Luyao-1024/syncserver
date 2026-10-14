//! Synthetic receiver used by the runnable example and focused durability tests.
//! This is not PhotoViewer's DB, dirty fields, publication layer or UI.
use rusqlite::{Connection, params};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub struct Receiver {
    pub db: Connection,
}
impl Receiver {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS progress(id INTEGER PRIMARY KEY CHECK(id=1),ingest TEXT,applied TEXT,target TEXT);INSERT OR IGNORE INTO progress(id) VALUES(1);CREATE TABLE IF NOT EXISTS inbox(id INTEGER PRIMARY KEY,revision TEXT UNIQUE NOT NULL,body TEXT NOT NULL);CREATE TABLE IF NOT EXISTS projection(name TEXT PRIMARY KEY,body TEXT NOT NULL);CREATE TABLE IF NOT EXISTS resource_work(asset_id TEXT NOT NULL,role TEXT NOT NULL,descriptor TEXT NOT NULL,ready INTEGER NOT NULL DEFAULT 0 CHECK(ready IN(0,1)),PRIMARY KEY(asset_id,role));")?;
        Ok(Self { db })
    }
    pub fn ingest(&self) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT ingest FROM progress WHERE id=1", [], |r| r.get(0))?)
    }
    pub fn applied(&self) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT applied FROM progress WHERE id=1", [], |r| r.get(0))?)
    }
    pub fn receive(&mut self, page: &Value) -> Result<()> {
        let token = page["syncToken"].as_str().ok_or("missing syncToken")?;
        let more = page["moreComing"].as_bool().ok_or("missing moreComing")?;
        let records = page["records"].as_array().ok_or("missing records")?;
        let tx = self.db.transaction()?;
        let prior: Option<String> =
            tx.query_row("SELECT ingest FROM progress WHERE id=1", [], |r| r.get(0))?;
        if prior.as_deref() == Some(token) {
            return Ok(());
        }
        for record in records {
            let revision = record["recordChangeTag"]
                .as_str()
                .ok_or("missing revision")?;
            tx.execute(
                "INSERT OR IGNORE INTO inbox(revision,body) VALUES(?1,?2)",
                params![revision, record.to_string()],
            )?;
        }
        tx.execute("UPDATE progress SET ingest=?1 WHERE id=1", [token])?;
        if !more {
            let rows = {
                let mut stmt = tx.prepare("SELECT body FROM inbox ORDER BY id")?;
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for body in rows {
                let record: Value = serde_json::from_str(&body)?;
                let name = record["recordName"].as_str().ok_or("missing recordName")?;
                if record["deleted"] == true {
                    tx.execute("DELETE FROM projection WHERE name=?1", [name])?;
                } else {
                    tx.execute("INSERT INTO projection VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET body=excluded.body",params![name,body])?;
                }
            }
            let rows = {
                let mut stmt = tx.prepare("SELECT body FROM projection ORDER BY name")?;
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            let records: Vec<Value> = rows
                .iter()
                .map(|body| serde_json::from_str(body))
                .collect::<std::result::Result<_, _>>()?;
            let names: BTreeMap<_, _> = records
                .iter()
                .map(|r| (r["recordName"].as_str().unwrap_or_default(), r))
                .collect();
            // A page can split an atomic photo pair. Do this only at final H.
            for record in &records {
                let fields = &record["fields"];
                match record["recordType"].as_str() {
                    Some("CPLAsset") => {
                        let partner = fields["masterRef"]["value"]["recordName"]
                            .as_str()
                            .ok_or("missing masterRef")?;
                        let master = names.get(partner).ok_or("incomplete photo pair")?;
                        if master["recordType"] != "CPLMaster"
                            || master["fields"]["photoId"]["value"] != record["recordName"]
                            || master["fields"]["contentGeneration"]["value"]
                                != fields["contentGeneration"]["value"]
                        {
                            return Err("mixed photo pair".into());
                        }
                    }
                    Some("CPLMaster") => {
                        let photo = fields["photoId"]["value"]
                            .as_str()
                            .and_then(|name| names.get(name))
                            .ok_or("incomplete photo pair")?;
                        if photo["recordType"] != "CPLAsset" {
                            return Err("invalid photo pair".into());
                        }
                    }
                    _ => {}
                }
            }
            let mut needed = std::collections::BTreeSet::new();
            for record in records {
                if let Some(fields) = record["fields"].as_object() {
                    for (name, field) in fields {
                        if field["type"] == "ASSETID" {
                            let role = if name == "resJPEGThumbRes" {
                                "thumbnail"
                            } else {
                                "original"
                            };
                            let descriptor = &field["value"];
                            let id = descriptor["assetId"].as_str().ok_or("missing assetId")?;
                            needed.insert((id.to_owned(), role.to_owned()));
                            tx.execute("INSERT INTO resource_work(asset_id,role,descriptor) VALUES(?1,?2,?3) ON CONFLICT(asset_id,role) DO UPDATE SET descriptor=excluded.descriptor",params![id,role,descriptor.to_string()])?;
                        }
                    }
                }
            }
            let obsolete = {
                let mut statement = tx.prepare("SELECT asset_id,role FROM resource_work")?;
                statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for (id, role) in obsolete {
                if !needed.contains(&(id.clone(), role.clone())) {
                    tx.execute(
                        "DELETE FROM resource_work WHERE asset_id=?1 AND role=?2",
                        params![id, role],
                    )?;
                }
            }
            tx.execute("DELETE FROM inbox", [])?;
            tx.execute("UPDATE progress SET target=?1 WHERE id=1", [token])?;
            tx.execute("UPDATE progress SET applied=target WHERE id=1 AND NOT EXISTS(SELECT 1 FROM resource_work WHERE ready=0)",[])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Vec<(String, String, Value)>> {
        let mut stmt=self.db.prepare("SELECT asset_id,role,descriptor FROM resource_work WHERE ready=0 ORDER BY asset_id,role")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, role, body)| Ok((id, role, serde_json::from_str(&body)?)))
            .collect()
    }
    /// Caller has fsynced and published a local immutable file whose actual
    /// digest/size it computed during streaming. Failed downloads never call this.
    #[allow(dead_code)]
    pub fn resource_published(
        &mut self,
        id: &str,
        role: &str,
        size: u64,
        checksum: &str,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        let body: String = tx.query_row(
            "SELECT descriptor FROM resource_work WHERE asset_id=?1 AND role=?2",
            params![id, role],
            |r| r.get(0),
        )?;
        let value: Value = serde_json::from_str(&body)?;
        if value["size"].as_u64() != Some(size) || value["fileChecksum"].as_str() != Some(checksum)
        {
            return Err("download verification failed".into());
        }
        tx.execute(
            "UPDATE resource_work SET ready=1 WHERE asset_id=?1 AND role=?2",
            params![id, role],
        )?;
        tx.execute("UPDATE progress SET applied=target WHERE id=1 AND NOT EXISTS(SELECT 1 FROM inbox) AND NOT EXISTS(SELECT 1 FROM resource_work WHERE ready=0)",[])?;
        tx.commit()?;
        Ok(())
    }
}
