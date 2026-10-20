use serde_json::{Value, json};
use syncserver::domain::{
    photos::{safe_relative_path, validate_graph},
    records::{self, ModifyRequest, Record},
};

fn fixture() -> (Vec<Record>, Value) {
    let fixtures: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let request = fixtures["createPhotoRequest"].clone();
    let prepared = serde_json::from_value::<ModifyRequest>(request.clone()).unwrap();
    // Use the public input and pure domain rules; no Published DB rows are forged.
    let records: Vec<_> = prepared
        .operations
        .iter()
        .map(|operation| {
            let op = operation;
            Record {
                events: Default::default(),
                name: records::RecordName(op.record.record_name.clone()),
                record_type: op.record.record_type.clone().unwrap(),
                zone: op.record.zone_id.clone().unwrap_or_default(),
                revision: records::Revision("current-tag".into()),
                seq: records::CommitSeq(1),
                fields: op
                    .record
                    .fields
                    .iter()
                    .map(|(name, field)| (name.clone(), field.clone().unwrap()))
                    .collect(),
                deleted: false,
            }
        })
        .collect();
    (records, request)
}
fn operations(request: Value) -> Vec<records::ResolvedOperation> {
    let request: ModifyRequest = serde_json::from_value(request).unwrap();
    request
        .operations
        .into_iter()
        .map(|operation| records::ResolvedOperation {
            events: None,
            operation_type: operation.operation_type,
            record_name: records::RecordName(operation.record.record_name),
            record_type: operation.record.record_type,
            zone: operation.record.zone_id.unwrap_or_default(),
            revision: operation.record.record_change_tag.map(records::Revision),
            fields: operation.record.fields,
        })
        .collect()
}
#[test]
fn frozen_photo_pair_is_valid_in_both_operation_orders_and_metadata_reuses_generation() {
    let (records, request) = fixture();
    let mut ops = operations(request);
    let bindings = validate_graph(&[], &records, &ops, true).unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].photo_id, "photo-001");
    ops.reverse();
    assert_eq!(validate_graph(&[], &records, &ops, true).unwrap(), bindings);
    let asset = records
        .iter()
        .find(|record| record.record_type == "CPLAsset")
        .unwrap();
    let op = operations(
        json!({"operations":[{"operationType":"update","record":{"recordName":asset.name.0,"recordChangeTag":asset.revision.0,"fields":{"isFavorite":{"type":"INT64","value":1}}}}]}),
    );
    let mut after = records.clone();
    let index = after
        .iter()
        .position(|record| record.record_type == "CPLAsset")
        .unwrap();
    after[index] = records::apply(&op[0], Some(asset)).unwrap();
    assert_eq!(
        validate_graph(&records, &after, &op, false).unwrap(),
        bindings
    );
}
#[test]
fn split_pairs_mixed_generation_dangling_references_invalid_fields_and_paths_fail() {
    let (records, request) = fixture();
    let ops = operations(request);
    for index in 0..2 {
        assert_eq!(
            validate_graph(
                &[],
                &records[index..index + 1],
                &ops[index..index + 1],
                true
            )
            .unwrap_err()
            .code,
            "PHOTO_PAIR_REQUIRED"
        );
    }
    assert_eq!(
        validate_graph(&[], &records, &ops, false).unwrap_err().code,
        "PHOTO_PAIR_REQUIRED"
    );
    for variant in 0..11 {
        let mut after = records.clone();
        let asset = after
            .iter_mut()
            .find(|record| record.record_type == "CPLAsset")
            .unwrap();
        match variant {
            0 => {
                asset.fields.get_mut("contentGeneration").unwrap().value =
                    json!(uuid::Uuid::new_v4())
            }
            1 => asset.fields.get_mut("masterRef").unwrap().value["recordName"] = json!("missing"),
            2 => {
                asset.fields.get_mut("masterRef").unwrap().value["zoneID"] =
                    json!({"zoneName":"Other"})
            }
            3 => asset.fields.get_mut("relativePath").unwrap().value = json!("../escape"),
            4 => asset.fields.get_mut("isFavorite").unwrap().value = json!(2),
            5 => asset.fields.get_mut("isHidden").unwrap().kind = "STRING".into(),
            9 => asset.fields.get_mut("isTrashed").unwrap().value = json!(2),
            10 => asset.fields.get_mut("isTrashed").unwrap().kind = "STRING".into(),
            6 => {
                after
                    .iter_mut()
                    .find(|record| record.record_type == "CPLMaster")
                    .unwrap()
                    .fields
                    .remove("resJPEGThumbRes");
            }
            7 => {
                after
                    .iter_mut()
                    .find(|record| record.record_type == "CPLMaster")
                    .unwrap()
                    .fields
                    .get_mut("thumbnailSourceOriginalSHA256")
                    .unwrap()
                    .value = json!("0".repeat(64))
            }
            _ => {
                after
                    .iter_mut()
                    .find(|record| record.record_type == "CPLMaster")
                    .unwrap()
                    .fields
                    .get_mut("resOriginalRes")
                    .unwrap()
                    .value["size"] = json!(true)
            }
        }
        assert!(
            validate_graph(&[], &after, &ops, true).is_err(),
            "variant {variant}"
        );
    }
    for path in [
        "",
        "/absolute",
        "a//b",
        "a/./b",
        "a/../b",
        "a\\b",
        "a\0b",
        ".syncserver/a",
        ".photoviewer-sync/a",
        ".sync-staging/a",
    ] {
        assert!(!safe_relative_path(path));
    }
    assert!(safe_relative_path("照片/春节 原图.png"));
}
#[test]
fn resource_changes_need_a_new_generation_and_two_conditional_updates() {
    let (before, request) = fixture();
    let mut ops = operations(request);
    for op in &mut ops {
        op.operation_type = "update".into();
        op.revision = Some(records::Revision("current-tag".into()));
    }
    let mut after = before.clone();
    after
        .iter_mut()
        .find(|record| record.record_type == "CPLMaster")
        .unwrap()
        .fields
        .get_mut("resOriginalRes")
        .unwrap()
        .value["assetId"] = json!(uuid::Uuid::new_v4());
    assert_eq!(
        validate_graph(&before, &after, &ops, true)
            .unwrap_err()
            .code,
        "PHOTO_PAIR_REQUIRED"
    );
    let new_generation = uuid::Uuid::new_v4();
    for record in &mut after {
        record.fields.get_mut("contentGeneration").unwrap().value = json!(new_generation);
    }
    assert_eq!(
        validate_graph(&before, &after, &ops, true).unwrap()[0].generation,
        new_generation
    );
    assert!(validate_graph(&before, &after, &ops, false).is_err());
    assert!(validate_graph(&before, &after, &ops[..1], true).is_err());
}
#[test]
fn paired_deletion_and_duplicate_paths_are_validated_on_the_final_graph() {
    let (before, request) = fixture();
    let mut ops = operations(request);
    let mut after = before.clone();
    for record in &mut after {
        record.deleted = true;
        record.fields.clear();
    }
    for op in &mut ops {
        op.operation_type = "delete".into();
        op.fields.clear();
    }
    ops.reverse();
    assert!(
        validate_graph(&before, &after, &ops, true)
            .unwrap()
            .is_empty()
    );
    assert!(validate_graph(&before, &after, &ops, false).is_err());
    let mut second = before.clone();
    for record in &mut second {
        if record.record_type == "CPLAsset" {
            record.name.0 = "photo-002".into();
            record.fields.get_mut("masterRef").unwrap().value["recordName"] = json!("master-002");
        } else {
            record.name.0 = "master-002".into();
            record.fields.get_mut("photoId").unwrap().value = json!("photo-002");
        }
    }
    let (mut all, request) = fixture();
    let mut all_ops = operations(request);
    let mut second_ops = all_ops.clone();
    for op in &mut second_ops {
        op.record_name.0 = if op.record_name.0 == "photo-001" {
            "photo-002"
        } else {
            "master-002"
        }
        .into();
    }
    all.extend(second);
    all_ops.extend(second_ops);
    assert_eq!(
        validate_graph(&[], &all, &all_ops, true).unwrap_err().code,
        "PATH_CONFLICT"
    );
}

#[test]
fn persistent_asset_references_do_not_depend_on_temporary_urls_or_receipts() {
    let (mut records, request) = fixture();
    let ops = operations(request);
    let expected = validate_graph(&[], &records, &ops, true).unwrap();
    for field in records
        .iter_mut()
        .flat_map(|record| record.fields.values_mut())
        .filter(|field| field.kind == "ASSETID")
    {
        let value = field.value.as_object_mut().unwrap();
        value.remove("receipt");
        value.remove("downloadURL");
    }
    assert_eq!(validate_graph(&[], &records, &ops, true).unwrap(), expected);
}
