//! The database owner enforces admission, phase and final-commit fencing.
use super::records::Outcome;
use crate::{
    Config, Error, Result,
    domain::{
        leases::{Acquire, Check, Control, LeaseClaims, LeaseProof, Phase},
        records::{Failure, Modify, ZoneId, identifier},
        sync::{Claims, Cursor, MAX_ZONES},
    },
    tokens::TokenCodec,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Instant};
use uuid::Uuid;
pub(crate) struct Clock {
    started: Instant,
    #[cfg(test)]
    advance_ms: i64,
}
impl Clock {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            #[cfg(test)]
            advance_ms: 0,
        }
    }
    pub fn tick(&self) -> Result<i64> {
        let tick =
            i64::try_from(self.started.elapsed().as_millis()).map_err(|_| Error::RecoveryFailed)?;
        #[cfg(test)]
        let tick = tick + self.advance_ms;
        Ok(tick)
    }
}
pub(crate) struct Guard<'a> {
    pub clock: &'a Clock,
    pub config: &'a Config,
    pub generation: Uuid,
    pub codec: &'a TokenCodec,
}
#[derive(Debug)]
pub(crate) struct Active {
    pub id: String,
    pub fence: i64,
    pub zones: Vec<ZoneId>,
    pub phase: String,
    pub verified: Option<i64>,
}
pub(crate) fn lost() -> Failure {
    Failure::new(
        409,
        "UPLOAD_LEASE_LOST",
        "Upload lease is absent, expired or replaced",
    )
}
pub(crate) fn mismatch() -> Failure {
    Failure::new(
        409,
        "UPLOAD_PHASE_MISMATCH",
        "Operation does not match the current upload phase",
    )
}
impl Guard<'_> {
    pub fn active(&self, db: &Connection, proof: &LeaseProof) -> Result<Active> {
        let claims: LeaseClaims = self
            .codec
            .unseal(&proof.token, "lease")
            .map_err(|_| lost())?;
        if claims.namespace != self.config.namespace
            || claims.generation != self.generation
            || claims.lease_id.to_string() != proof.id
            || claims.fence != proof.fence
        {
            return Err(lost().into());
        }
        let row=db.query_row("SELECT lease_id,device_id,fence,zones_json,deadline_tick_ms,phase,verified_head FROM upload_lease WHERE namespace_id=1 AND lease_id IS NOT NULL",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<i64>>(6)?))).optional()?;
        let Some((id, device, fence, zones, deadline, phase, verified)) = row else {
            return Err(lost().into());
        };
        let zones: Vec<ZoneId> = serde_json::from_str(&zones).map_err(|_| Error::RecoveryFailed)?;
        if id != proof.id
            || fence.to_string() != proof.fence
            || device != claims.device_id
            || zones != claims.zones
            || deadline <= self.clock.tick()?
        {
            return Err(lost().into());
        }
        Ok(Active {
            id,
            fence,
            zones,
            phase,
            verified,
        })
    }
    pub fn mutation(&self, db: &Connection, request: &Modify) -> Result<()> {
        let proof = request.lease.as_ref().ok_or_else(lost)?;
        let active = self.active(db, proof)?;
        if active.verified.is_none() {
            return Err(Failure::new(
                409,
                "SYNC_NOT_CURRENT",
                "Complete current zone progress is required before uploads",
            )
            .into());
        }
        for op in &request.operations {
            let expected = match op.operation_type.as_str() {
                "delete" => "delete",
                "update" => "modify",
                "create" => "create",
                _ => return Err(Failure::unsupported().into()),
            };
            if active.phase != expected {
                return Err(mismatch().into());
            }
            if !active.zones.contains(&op.zone) {
                return Err(Failure::bad().into());
            }
        }
        Ok(())
    }
    fn head(&self, db: &Connection) -> Result<i64> {
        Ok(db.query_row(
            "SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1",
            [],
            |r| r.get(0),
        )?)
    }
    fn deadline(&self) -> Result<(i64, i64)> {
        let ttl = i64::try_from(self.config.limits.lease_ttl_secs * 1000)
            .map_err(|_| Error::RecoveryFailed)?;
        Ok((
            self.clock
                .tick()?
                .checked_add(ttl)
                .ok_or(Error::RecoveryFailed)?,
            crate::jobs::now_ms()?
                .checked_add(ttl)
                .ok_or(Error::RecoveryFailed)?,
        ))
    }
    fn acquire(&self, db: &mut Connection, request: Acquire) -> Result<Value> {
        if !identifier(&request.device_id)
            || request.zones.is_empty()
            || request.zones.len() > MAX_ZONES
        {
            return Err(Failure::bad().into());
        }
        let mut names = BTreeSet::new();
        for zone in &request.zones {
            zone.validate()?;
            if !self.config.zones.contains(&zone.zone_name) || !names.insert(&zone.zone_name) {
                return Err(Failure::bad().into());
            }
        }
        let tx = db.transaction()?;
        let (fence, deadline): (i64, Option<i64>) = tx.query_row(
            "SELECT fence,deadline_tick_ms FROM upload_lease WHERE namespace_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let now = self.clock.tick()?;
        if deadline.is_some_and(|deadline| deadline > now) {
            return Err(Failure::new(
                409,
                "UPLOAD_LOCK_BUSY",
                "Another device holds the upload lease",
            )
            .into());
        }
        let fence = fence.checked_add(1).ok_or(Error::RecoveryFailed)?;
        let id = Uuid::new_v4();
        let (tick, expires) = self.deadline()?;
        let claims = LeaseClaims {
            namespace: self.config.namespace.clone(),
            generation: self.generation,
            lease_id: id,
            device_id: request.device_id.clone(),
            fence: fence.to_string(),
            zones: request.zones.clone(),
        };
        let token = self.codec.seal("lease", &claims)?;
        tx.execute("UPDATE upload_lease SET fence=?1,lease_id=?2,device_id=?3,zones_json=?4,deadline_tick_ms=?5,expires_at_ms=?6,phase='checking',verified_head=NULL WHERE namespace_id=1",params![fence,id.to_string(),request.device_id,serde_json::to_string(&request.zones).map_err(|_|Error::RecoveryFailed)?,tick,expires])?;
        tx.commit()?;
        Ok(
            json!({"leaseId":id,"leaseToken":token,"fencingToken":fence.to_string(),"expiresAtMs":expires,"ttlSeconds":self.config.limits.lease_ttl_secs,"heartbeatSeconds":self.config.limits.lease_heartbeat_secs,"phase":"checking","zoneIDs":request.zones}),
        )
    }
    fn check(&self, db: &mut Connection, proof: LeaseProof, request: Check) -> Result<Value> {
        let tx = db.transaction()?;
        let active = self.active(&tx, &proof)?;
        if active.phase != "checking" {
            return Err(mismatch().into());
        }
        if request.zone_tokens.len() != active.zones.len() {
            return Err(Failure::bad().into());
        }
        let head = self.head(&tx)?;
        let mut seen = BTreeSet::new();
        let mut current = true;
        let mut tokens = Vec::new();
        for item in request.zone_tokens {
            if !active.zones.contains(&item.zone) || !seen.insert(item.zone.zone_name.clone()) {
                return Err(Failure::bad().into());
            }
            let claims = self.codec.decode(&item.sync_token, "zone")?;
            if claims.namespace != self.config.namespace || claims.zone.as_ref() != Some(&item.zone)
            {
                return Err(Failure::bad().into());
            }
            let Cursor::Zone {
                position,
                watermark,
            } = claims.cursor
            else {
                return Err(Failure::bad().into());
            };
            if watermark.is_some() || position < 0 {
                return Err(Failure::bad().into());
            }
            if claims.generation != self.generation || position > head {
                return Err(Failure::new(
                    400,
                    "EXPIRED_SYNC_TOKEN",
                    "Token generation or position expired",
                )
                .into());
            }
            let behind:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM changes WHERE namespace_id=1 AND zone_name=?1 AND seq>?2 AND seq<=?3)",params![item.zone.zone_name,position,head],|r|r.get(0))?;
            current &= !behind;
            let token = self.codec.encode(&Claims {
                version: 1,
                namespace: self.config.namespace.clone(),
                generation: self.generation,
                zone: Some(item.zone.clone()),
                cursor: Cursor::Zone {
                    position: head,
                    watermark: None,
                },
            })?;
            tokens.push(json!({"zoneID":item.zone,"syncToken":token}));
        }
        let may_upload = current && request.local_ready;
        self.active(&tx, &proof)?;
        if may_upload {
            tx.execute(
                "UPDATE upload_lease SET verified_head=?1,phase='delete' WHERE namespace_id=1",
                [head],
            )?;
        }
        tx.commit()?;
        Ok(
            json!({"isCurrent":current,"mayUpload":may_upload,"zoneTokens":tokens,"phase":if may_upload{"delete"}else{"checking"}}),
        )
    }
    pub fn control(&self, db: &mut Connection, request: Control) -> Result<Outcome> {
        let body = match request {
            Control::Acquire(r) => self.acquire(db, r)?,
            Control::Check(proof, r) => self.check(db, proof, r)?,
            Control::Renew(proof) => {
                let tx = db.transaction()?;
                self.active(&tx, &proof)?;
                let (tick, expires) = self.deadline()?;
                tx.execute("UPDATE upload_lease SET deadline_tick_ms=?1,expires_at_ms=?2 WHERE namespace_id=1",params![tick,expires])?;
                tx.commit()?;
                json!({"leaseId":proof.id,"expiresAtMs":expires,"ttlSeconds":self.config.limits.lease_ttl_secs})
            }
            Control::Release(proof) => {
                let tx = db.transaction()?;
                self.active(&tx, &proof)?;
                tx.execute("UPDATE upload_lease SET lease_id=NULL,device_id=NULL,zones_json=NULL,deadline_tick_ms=NULL,expires_at_ms=NULL,phase=NULL,verified_head=NULL WHERE namespace_id=1",[])?;
                tx.commit()?;
                json!({"released":true})
            }
            Control::Phase(proof, Phase { phase }) => {
                let tx = db.transaction()?;
                let active = self.active(&tx, &proof)?;
                if active.verified.is_none() {
                    return Err(Failure::new(
                        409,
                        "SYNC_NOT_CURRENT",
                        "Progress gate has not passed",
                    )
                    .into());
                }
                let rank = |phase: &str| match phase {
                    "delete" => Some(0),
                    "modify" => Some(1),
                    "create" => Some(2),
                    _ => None,
                };
                let from = rank(&active.phase).ok_or_else(mismatch)?;
                let to = rank(&phase).ok_or_else(Failure::bad)?;
                if to < from || to > from + 1 {
                    return Err(mismatch().into());
                }
                let receiving:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM upload_sessions WHERE namespace_id=1 AND lease_id=?1 AND fence=?2 AND state IN('Receiving','PublishPrepared'))",params![proof.id,active.fence],|r|r.get(0))?;
                if receiving {
                    return Err(Failure::new(
                        409,
                        "UPLOAD_BUSY",
                        "Settle current uploads before advancing phase",
                    )
                    .into());
                }
                tx.execute(
                    "UPDATE upload_lease SET phase=?1 WHERE namespace_id=1",
                    [&phase],
                )?;
                self.active(&tx, &proof)?;
                tx.commit()?;
                json!({"phase":phase})
            }
            Control::Status(id) => {
                if !identifier(&id) || id.len() > 128 {
                    return Err(Failure::bad().into());
                }
                let mut stmt=db.prepare("SELECT item_index,result_json FROM operation_receipts WHERE namespace_id=1 AND operation_id=?1 ORDER BY item_index")?;
                let entries = stmt
                    .query_map([&id], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if entries.is_empty() {
                    return Err(Failure::unknown().into());
                }
                let mut result = None;
                let mut items = Vec::new();
                for (index, value) in entries {
                    let value = value
                        .map(|v| {
                            serde_json::from_str::<Value>(&v).map_err(|_| Error::RecoveryFailed)
                        })
                        .transpose()?;
                    if index == -1 {
                        result = value;
                    } else {
                        items.push(json!({"index":index,"result":value}));
                    }
                }
                json!({"operationId":id,"state":if result.is_some(){"Committed"}else{"Pending"},"result":result,"items":items})
            }
        };
        Ok(Outcome::success(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monotonic_expiration_cannot_be_renewed_or_release_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::from_pairs([
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_API_KEY_FILE", "/unused/api"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
        ])
        .unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        let generation = crate::store::migrations::initialize(&mut db, &config, &[1; 32]).unwrap();
        let codec = TokenCodec::new([1; 32]);
        let mut clock = Clock::new();
        let acquired = Guard {
            clock: &clock,
            config: &config,
            generation,
            codec: &codec,
        }
        .control(
            &mut db,
            Control::Acquire(Acquire {
                device_id: "a".into(),
                zones: vec![ZoneId::default()],
            }),
        )
        .unwrap()
        .body;
        let proof = LeaseProof {
            id: acquired["leaseId"].as_str().unwrap().into(),
            token: acquired["leaseToken"].as_str().unwrap().into(),
            fence: acquired["fencingToken"].as_str().unwrap().into(),
        };
        clock.advance_ms = 90_001;
        let guard = Guard {
            clock: &clock,
            config: &config,
            generation,
            codec: &codec,
        };
        assert!(
            matches!(guard.control(&mut db,Control::Renew(proof.clone())),Err(Error::Protocol(e))if e.code=="UPLOAD_LEASE_LOST")
        );
        let replacement = guard
            .control(
                &mut db,
                Control::Acquire(Acquire {
                    device_id: "b".into(),
                    zones: vec![ZoneId::default()],
                }),
            )
            .unwrap()
            .body;
        assert!(
            replacement["fencingToken"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > proof.fence.parse::<u64>().unwrap()
        );
        assert!(guard.control(&mut db, Control::Release(proof)).is_err());
        assert!(
            guard
                .control(
                    &mut db,
                    Control::Renew(LeaseProof {
                        id: replacement["leaseId"].as_str().unwrap().into(),
                        token: replacement["leaseToken"].as_str().unwrap().into(),
                        fence: replacement["fencingToken"].as_str().unwrap().into()
                    })
                )
                .is_ok()
        );
    }
}
