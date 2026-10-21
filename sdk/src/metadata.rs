//! Metadata reads and conditional writes.
//!
//! Snapshot tokens, continuation markers and per-zone sync tokens are opaque:
//! their only meaning is "hand this back". This module never parses or compares
//! them; it exposes them as strings the caller can persist.

use crate::Error;
use crate::client::{Connection, decode};
use crate::lease::LeaseProof;
use crate::model::{
    ASSET_RECORD_TYPE, MASTER_RECORD_TYPE, ModifyRequest, RecordResult, RemoteRecord, ZoneId,
};
use crate::photo::{PhotoGraph, RemotePhoto};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(rename = "zoneID")]
    pub zone: ZoneId,
    pub snapshot_token: String,
    pub sync_token: String,
}

/// One page of records. `failures` carries in-band item errors, which the
/// service may return inside an HTTP 200.
#[derive(Clone, Debug, Default)]
pub struct RecordPage {
    pub records: Vec<RemoteRecord>,
    pub failures: Vec<crate::ServerFailure>,
    pub sync_token: String,
    pub continuation_marker: Option<String>,
    /// True when more pages exist. Derived from `continuationMarker` for queries
    /// and from `moreComing` for change feeds.
    pub more_coming: bool,
}

/// A query request. `snapshot_token` fixes the read at one height; without it
/// the query reads the current head and cannot be combined with other pages.
#[derive(Clone, Debug)]
pub struct QueryRequest {
    pub zone: ZoneId,
    pub record_type: String,
    pub results_limit: Option<usize>,
    pub desired_keys: Option<Vec<String>>,
    pub snapshot_token: Option<String>,
    pub continuation_marker: Option<String>,
}

impl QueryRequest {
    pub fn new(zone: ZoneId, record_type: impl Into<String>) -> Self {
        Self {
            zone,
            record_type: record_type.into(),
            results_limit: None,
            desired_keys: None,
            snapshot_token: None,
            continuation_marker: None,
        }
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.results_limit = Some(limit);
        self
    }

    pub fn desired_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.desired_keys = Some(keys.into_iter().map(Into::into).collect());
        self
    }

    pub fn snapshot(mut self, token: impl Into<String>) -> Self {
        self.snapshot_token = Some(token.into());
        self
    }

    pub fn continuation(mut self, marker: impl Into<String>) -> Self {
        self.continuation_marker = Some(marker.into());
        self
    }
}

/// The result of reading every page of one record type at one snapshot height.
#[derive(Clone, Debug)]
pub struct QueryAll {
    pub records: Vec<RemoteRecord>,
    pub sync_token: String,
}

#[derive(Clone, Debug)]
pub struct Lookup {
    pub records: Vec<RecordResult>,
    pub sync_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneChange {
    #[serde(rename = "zoneID")]
    pub zone: ZoneId,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseChanges {
    pub zones: Vec<ZoneChange>,
    pub more_coming: bool,
    pub sync_token: String,
}

/// One zone's incremental feed request. The first call for a zone omits the
/// token; every later call carries the token from the previous page.
#[derive(Clone, Debug)]
pub struct ZoneChangesRequest {
    pub zone: ZoneId,
    pub sync_token: Option<String>,
    pub desired_record_types: Option<Vec<String>>,
    pub desired_keys: Option<Vec<String>>,
    pub results_limit: Option<usize>,
}

impl ZoneChangesRequest {
    pub fn new(zone: ZoneId) -> Self {
        Self {
            zone,
            sync_token: None,
            desired_record_types: None,
            desired_keys: None,
            results_limit: None,
        }
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.sync_token = Some(token.into());
        self
    }

    pub fn record_types(mut self, types: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.desired_record_types = Some(types.into_iter().map(Into::into).collect());
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.results_limit = Some(limit);
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChangesPage {
    pub zone: Option<ZoneId>,
    pub records: Vec<RemoteRecord>,
    pub failures: Vec<crate::ServerFailure>,
    pub more_coming: bool,
    pub sync_token: String,
}

/// A per-zone entry of a multi-zone change read. A zone can fail in-band while
/// the other zones in the same response succeed.
#[derive(Clone, Debug)]
pub enum ZoneChangeOutcome {
    Changed(ChangesPage),
    Failed(crate::ServerFailure),
}

/// A complete snapshot of one zone's photo graph.
#[derive(Clone, Debug)]
pub struct Bootstrap {
    pub zone: ZoneId,
    pub snapshot_token: String,
    pub sync_token: String,
    pub masters: Vec<RemoteRecord>,
    pub assets: Vec<RemoteRecord>,
    pub photos: Vec<RemotePhoto>,
}

/// The state an idempotent operation left behind, read back through its receipt.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationState {
    Committed(Value),
    /// Still running. Committed child results are evidence; the operation as a
    /// whole is neither a success nor a failure yet.
    Pending(Vec<Value>),
    /// No receipt exists under this id. For an operation issued without an
    /// idempotency key this is the expected answer and says nothing about
    /// whether it committed.
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPage {
    #[serde(default)]
    records: Vec<Value>,
    #[serde(rename = "syncToken", default)]
    sync_token: String,
    #[serde(rename = "continuationMarker", default)]
    continuation_marker: Option<String>,
    #[serde(rename = "moreComing", default)]
    more_coming: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLookup {
    #[serde(default)]
    records: Vec<Value>,
    #[serde(rename = "syncToken", default)]
    sync_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDatabaseChanges {
    #[serde(default)]
    zones: Vec<ZoneChange>,
    #[serde(rename = "moreComing", default)]
    more_coming: bool,
    #[serde(rename = "syncToken", default)]
    sync_token: String,
}

#[derive(Deserialize)]
struct RawZones {
    #[serde(default)]
    zones: Vec<Value>,
}

impl Connection {
    fn page_limit(&self, requested: Option<usize>) -> Result<usize, Error> {
        let announced = self.scope().sync_limits.max_page;
        let maximum = if announced == 0 {
            200
        } else {
            announced.clamp(1, 200)
        };
        let limit = requested.unwrap_or_else(|| self.scope().page_limit());
        if !(1..=maximum).contains(&limit) {
            return Err(Error::Protocol(format!(
                "resultsLimit must be between 1 and {maximum}"
            )));
        }
        Ok(limit)
    }

    fn max_zones(&self) -> usize {
        let announced = self.scope().sync_limits.max_zones;
        if announced == 0 {
            20
        } else {
            announced.clamp(1, 20)
        }
    }

    pub async fn snapshot(&self, zone: &ZoneId) -> Result<Snapshot, Error> {
        self.check_zone(zone)?;
        let url = self.url(&["v1", "snapshots"])?;
        let body = self
            .client()
            .post_json_bytes(&url, &json!({ "zoneID": zone }), None, None)
            .await?;
        decode(&body)
    }

    pub async fn query_page(&self, request: &QueryRequest) -> Result<RecordPage, Error> {
        self.check_zone(&request.zone)?;
        if !crate::model::identifier(&request.record_type) {
            return Err(Error::Protocol("invalid record type".into()));
        }
        let limit = self.page_limit(request.results_limit)?;
        if let Some(keys) = &request.desired_keys
            && (keys.len() > 256 || keys.iter().any(|key| !crate::model::identifier(key)))
        {
            return Err(Error::Protocol("invalid desiredKeys".into()));
        }
        let mut body = json!({
            "zoneID": request.zone,
            "query": { "recordType": request.record_type },
            "resultsLimit": limit,
        });
        if let Some(keys) = &request.desired_keys {
            body["desiredKeys"] = json!(keys);
        }
        if let Some(token) = &request.snapshot_token {
            body["snapshotToken"] = json!(token);
        }
        if let Some(marker) = &request.continuation_marker {
            body["continuationMarker"] = json!(marker);
        }
        // The server ignores the value but requires the field's type when present.
        body["getCurrentSyncToken"] = json!(true);
        let url = self.cloudkit_url("records/query")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, None, None)
            .await?;
        let raw: RawPage = decode(&bytes)?;
        let (records, failures) = split_records(raw.records)?;
        Ok(RecordPage {
            more_coming: raw.continuation_marker.is_some() || raw.more_coming,
            records,
            failures,
            sync_token: raw.sync_token,
            continuation_marker: raw.continuation_marker,
        })
    }

    /// Read every page of one record type at one snapshot height. In-band item
    /// failures make the result incomplete and are returned as errors rather
    /// than silently dropped.
    pub async fn query_all_records(
        &self,
        zone: &ZoneId,
        record_type: &str,
        snapshot_token: &str,
    ) -> Result<QueryAll, Error> {
        self.query_all_with_keys(zone, record_type, snapshot_token, None)
            .await
    }

    pub async fn query_all_with_keys(
        &self,
        zone: &ZoneId,
        record_type: &str,
        snapshot_token: &str,
        desired_keys: Option<Vec<String>>,
    ) -> Result<QueryAll, Error> {
        let mut request = QueryRequest::new(zone.clone(), record_type)
            .snapshot(snapshot_token)
            .limit(self.scope().page_limit());
        if let Some(keys) = desired_keys {
            request.desired_keys = Some(keys);
        }
        let mut records = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let sync_token = loop {
            let page = self.query_page(&request).await?;
            if let Some(failure) = page.failures.first() {
                return Err(Error::InBand(Box::new(failure.clone())));
            }
            records.extend(page.records);
            match page.continuation_marker {
                Some(marker) if seen.insert(marker.clone()) => {
                    request.continuation_marker = Some(marker);
                }
                Some(_) => {
                    return Err(Error::Protocol(
                        "the service repeated a continuation marker".into(),
                    ));
                }
                None => break page.sync_token,
            }
        };
        Ok(QueryAll {
            records,
            sync_token,
        })
    }

    pub async fn lookup_current(&self, zone: &ZoneId, names: &[&str]) -> Result<Lookup, Error> {
        self.lookup(zone, names, None).await
    }

    /// Look records up as they existed at a snapshot height. Tombstoned or
    /// never-existing names yield an item failure, not an absent entry.
    pub async fn lookup_snapshot(
        &self,
        zone: &ZoneId,
        snapshot_token: &str,
        names: &[&str],
    ) -> Result<Lookup, Error> {
        self.lookup(zone, names, Some(snapshot_token)).await
    }

    async fn lookup(
        &self,
        zone: &ZoneId,
        names: &[&str],
        snapshot_token: Option<&str>,
    ) -> Result<Lookup, Error> {
        self.check_zone(zone)?;
        if names.is_empty() || names.len() > crate::model::MAX_OPERATIONS {
            return Err(Error::Protocol(
                "a lookup needs 1..=400 record names".into(),
            ));
        }
        if names.iter().any(|name| !crate::model::identifier(name)) {
            return Err(Error::Protocol("invalid record name".into()));
        }
        let mut body = json!({
            "zoneID": zone,
            "records": names.iter().map(|name| json!({ "recordName": name })).collect::<Vec<_>>(),
        });
        if let Some(token) = snapshot_token {
            body["snapshotToken"] = json!(token);
        }
        let url = self.cloudkit_url("records/lookup")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, None, None)
            .await?;
        let raw: RawLookup = decode(&bytes)?;
        if raw.records.len() != names.len() {
            return Err(Error::Protocol(format!(
                "lookup returned {} of {} requested records",
                raw.records.len(),
                names.len()
            )));
        }
        let records = raw
            .records
            .iter()
            .map(RecordResult::from_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Lookup {
            records,
            sync_token: raw.sync_token,
        })
    }

    pub async fn changes_database(
        &self,
        sync_token: Option<&str>,
        results_limit: Option<usize>,
    ) -> Result<DatabaseChanges, Error> {
        let limit = self.page_limit(results_limit)?;
        let mut body = json!({ "resultsLimit": limit });
        if let Some(token) = sync_token {
            body["syncToken"] = json!(token);
        }
        let url = self.cloudkit_url("changes/database")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, None, None)
            .await?;
        let raw: RawDatabaseChanges = decode(&bytes)?;
        Ok(DatabaseChanges {
            zones: raw.zones,
            more_coming: raw.more_coming,
            sync_token: raw.sync_token,
        })
    }

    pub async fn changes_zone(&self, request: &ZoneChangesRequest) -> Result<ChangesPage, Error> {
        let mut outcomes = self.changes_zones(std::slice::from_ref(request)).await?;
        match outcomes.pop() {
            Some(ZoneChangeOutcome::Changed(page)) => Ok(page),
            Some(ZoneChangeOutcome::Failed(failure)) => Err(Error::InBand(Box::new(failure))),
            None => Err(Error::Protocol("change read returned no zones".into())),
        }
    }

    pub async fn changes_zones(
        &self,
        requests: &[ZoneChangesRequest],
    ) -> Result<Vec<ZoneChangeOutcome>, Error> {
        if requests.is_empty() || requests.len() > self.max_zones() {
            return Err(Error::Protocol(format!(
                "a change read needs 1..={} zones",
                self.max_zones()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut zones = Vec::with_capacity(requests.len());
        for request in requests {
            self.check_zone(&request.zone)?;
            if !seen.insert(request.zone.zone_name.clone()) {
                return Err(Error::Protocol("a zone may appear only once".into()));
            }
            let mut body = json!({
                "zoneID": request.zone,
                "resultsLimit": self.page_limit(request.results_limit)?,
            });
            if let Some(token) = &request.sync_token {
                body["syncToken"] = json!(token);
            }
            if let Some(types) = &request.desired_record_types {
                body["desiredRecordTypes"] = json!(types);
            }
            if let Some(keys) = &request.desired_keys {
                body["desiredKeys"] = json!(keys);
            }
            zones.push(body);
        }
        let url = self.cloudkit_url("changes/zone")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &json!({ "zones": zones }), None, None)
            .await?;
        let raw: RawZones = decode(&bytes)?;
        raw.zones
            .iter()
            .map(parse_zone_outcome)
            .collect::<Result<Vec<_>, _>>()
    }

    /// Read a zone's complete photo graph at one fixed height.
    pub async fn bootstrap(&self, zone: &ZoneId) -> Result<Bootstrap, Error> {
        let snapshot = self.snapshot(zone).await?;
        let masters = self
            .query_all_records(zone, MASTER_RECORD_TYPE, &snapshot.snapshot_token)
            .await?;
        let assets = self
            .query_all_records(zone, ASSET_RECORD_TYPE, &snapshot.snapshot_token)
            .await?;
        if masters.sync_token != assets.sync_token {
            return Err(Error::Protocol(
                "the two snapshot halves did not end at the same token".into(),
            ));
        }
        let graph = PhotoGraph::from_records(masters.records.iter().chain(assets.records.iter()))?;
        Ok(Bootstrap {
            zone: zone.clone(),
            snapshot_token: snapshot.snapshot_token,
            sync_token: masters.sync_token,
            masters: masters.records,
            assets: assets.records,
            photos: graph.photos,
        })
    }

    /// Read one photo and its master at the current head. The graph rules are
    /// applied to the pair, so an inconsistent pair is rejected rather than
    /// returned as a usable photo.
    pub async fn fetch_photo(&self, zone: &ZoneId, photo_id: &str) -> Result<RemotePhoto, Error> {
        let asset = self.confirmed_lookup(zone, photo_id).await?;
        if asset.record_type.as_deref() != Some(ASSET_RECORD_TYPE) {
            return Err(Error::Protocol(format!(
                "record {photo_id:?} is not a CPLAsset"
            )));
        }
        let master_name = asset
            .reference_name("masterRef")
            .ok_or_else(|| Error::Protocol(format!("CPLAsset {photo_id:?} has no masterRef")))?;
        let master = self.confirmed_lookup(zone, master_name).await?;
        if master.record_type.as_deref() != Some(MASTER_RECORD_TYPE) {
            return Err(Error::Protocol(format!(
                "record {master_name:?} is not a CPLMaster"
            )));
        }
        let photo = PhotoGraph::from_records([&asset, &master])?
            .photos
            .into_iter()
            .next()
            .ok_or_else(|| Error::Protocol("the pair did not yield a photo".into()))?;
        Ok(photo)
    }

    async fn confirmed_lookup(&self, zone: &ZoneId, name: &str) -> Result<RemoteRecord, Error> {
        let mut lookup = self.lookup_current(zone, &[name]).await?;
        match lookup.records.pop() {
            Some(RecordResult::Confirmed(record)) => Ok(record),
            Some(RecordResult::Failed(failure)) => Err(Error::InBand(Box::new(failure))),
            None => Err(Error::NotFound(name.to_string())),
        }
    }

    /// Issue a conditional mutation under a held lease. A returned item may
    /// still be a failure: HTTP 200 is not proof that every record committed.
    pub async fn modify(
        &self,
        lease: &LeaseProof,
        request: ModifyRequest,
    ) -> Result<Vec<RecordResult>, Error> {
        self.check_zone(&request.zone)?;
        let expected = request.operations.len();
        let key = request.idempotency_key.clone();
        let body = request.wire()?;
        let url = self.cloudkit_url("records/modify")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, Some(lease), key.as_deref())
            .await?;
        let value: Value = decode(&bytes)?;
        if value.get("serverErrorCode").is_some() {
            let failure: crate::ServerFailure = serde_json::from_value(value)
                .map_err(|error| Error::Protocol(format!("invalid error envelope: {error}")))?;
            return Err(Error::InBand(Box::new(failure)));
        }
        let records = value
            .get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Protocol("mutation returned no records array".into()))?;
        if records.len() != expected {
            return Err(Error::Protocol(format!(
                "mutation returned {} results for {expected} operations",
                records.len()
            )));
        }
        records
            .iter()
            .map(RecordResult::from_value)
            .collect::<Result<Vec<_>, _>>()
    }

    /// Submit a mutation envelope the caller built and persisted itself.
    ///
    /// This exists for clients that freeze one logical request in local storage
    /// before sending it and must later resend exactly that request under the
    /// same key. Prefer [`Connection::modify`]: it validates operations and
    /// parses per-record results. Here the caller owns the envelope shape, but
    /// the SDK still owns authentication, the lease headers, idempotency and
    /// the shared error mapping.
    pub async fn modify_exact(
        &self,
        lease: &LeaseProof,
        body: &Value,
        idempotency_key: &str,
    ) -> Result<Value, Error> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || !idempotency_key
                .bytes()
                .all(|byte| (33..=126).contains(&byte))
        {
            return Err(Error::Protocol("invalid idempotency key".into()));
        }
        if body.get("operations").and_then(Value::as_array).is_none() {
            return Err(Error::Protocol(
                "a mutation envelope needs an operations array".into(),
            ));
        }
        let url = self.cloudkit_url("records/modify")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, body, Some(lease), Some(idempotency_key))
            .await?;
        let value: Value = decode(&bytes)?;
        if value.get("serverErrorCode").is_some() {
            let failure: crate::ServerFailure = serde_json::from_value(value)
                .map_err(|error| Error::Protocol(format!("invalid error envelope: {error}")))?;
            return Err(Error::InBand(Box::new(failure)));
        }
        Ok(value)
    }

    /// Read back the receipt of an operation issued with this idempotency key.
    /// This call has no lease dependency, so it works after the lease is gone.
    pub async fn operation_status(&self, operation_id: &str) -> Result<OperationState, Error> {
        if !crate::model::identifier(operation_id) || operation_id.len() > 128 {
            return Err(Error::Protocol("invalid operation id".into()));
        }
        let url = self.url(&["v1", "operations", operation_id])?;
        let body = match self.client().get_bytes(&url).await {
            Ok(body) => body,
            Err(Error::NotFound(_)) => return Ok(OperationState::Unknown),
            Err(error) => return Err(error),
        };
        let value: Value = decode(&body)?;
        let items: Vec<Value> = value
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("result").cloned())
                    .filter(|result| !result.is_null())
                    .collect()
            })
            .unwrap_or_default();
        match value.get("state").and_then(Value::as_str) {
            Some("Committed") => Ok(OperationState::Committed(
                value.get("result").cloned().unwrap_or(Value::Null),
            )),
            Some("Pending") => Ok(OperationState::Pending(items)),
            Some(other) => Err(Error::Protocol(format!(
                "operation status reported unknown state {other:?}"
            ))),
            None => Err(Error::Protocol("operation status carried no state".into())),
        }
    }
}

fn split_records(
    values: Vec<Value>,
) -> Result<(Vec<RemoteRecord>, Vec<crate::ServerFailure>), Error> {
    let mut records = Vec::new();
    let mut failures = Vec::new();
    for value in values {
        match RecordResult::from_value(&value)? {
            RecordResult::Confirmed(record) => records.push(record),
            RecordResult::Failed(failure) => failures.push(failure),
        }
    }
    Ok((records, failures))
}

fn parse_zone_outcome(value: &Value) -> Result<ZoneChangeOutcome, Error> {
    if value.get("serverErrorCode").is_some() {
        let failure: crate::ServerFailure = serde_json::from_value(value.clone())
            .map_err(|error| Error::Protocol(format!("invalid zone failure: {error}")))?;
        return Ok(ZoneChangeOutcome::Failed(failure));
    }
    let raw: RawPage = serde_json::from_value(value.clone())
        .map_err(|error| Error::Protocol(format!("invalid zone change page: {error}")))?;
    let zone = value
        .get("zoneID")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| Error::Protocol(format!("invalid zoneID: {error}")))?;
    let (records, failures) = split_records(raw.records)?;
    Ok(ZoneChangeOutcome::Changed(ChangesPage {
        zone,
        records,
        failures,
        more_coming: raw.more_coming,
        sync_token: raw.sync_token,
    }))
}
