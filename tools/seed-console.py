#!/usr/bin/env python3
"""Populate a local run with accounts and photos so the console has something to show.

This is a development convenience, not part of the service: it speaks the same HTTP
contract a client does, including the upload lease, so anything it creates is exactly
what the server would accept from the real client.

    python3 tools/seed-console.py http://127.0.0.1:8790 --admin admin --password ...
"""

import argparse
import base64
import hashlib
import io
import json
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zlib

CONTAINER = "com.apple.photos.cloud"
ZONE = "PrimarySync"


def call(base, path, token=None, body=None, method=None, raw=None, content_type=None, headers=None):
    url = base.rstrip("/") + path
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Accept", "application/json")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    if content_type:
        request.add_header("Content-Type", content_type)
    elif body is not None:
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request) as response:
            payload = response.read()
            return json.loads(payload) if payload else None
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        raise SystemExit(f"{method or 'GET'} {path} failed: {error.code} {detail}") from None


def png(width, height, seed):
    """A real, decodable RGB PNG, because the server validates thumbnails."""

    def chunk(kind, payload):
        # A PNG chunk is length + type + data + CRC over type and data.
        head = kind + payload
        return (
            struct.pack(">I", len(payload))
            + head
            + struct.pack(">I", zlib.crc32(head) & 0xFFFFFFFF)
        )

    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            # A soft diagonal gradient with a per-image tint, so the gallery is
            # visually distinguishable rather than a wall of identical grey squares.
            base = (x * 5 + y * 3 + seed * 37) % 256
            rows += bytes(
                [
                    (base + seed * 23) % 256,
                    (base * 2 + 60 + seed * 11) % 256,
                    (255 - base + seed * 17) % 256,
                ]
            )

    header = b"\x89PNG\r\n\x1a\n"
    header += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    header += chunk(b"IDAT", zlib.compress(bytes(rows), 6))
    header += chunk(b"IEND", b"")
    return header


def login(base, username, password):
    body = call(base, "/v1/auth/login", body={"username": username, "password": password})
    return body["accessToken"]


def upload_photo(base, token, lease, run, index, original, thumbnail, path, taken_ms, favorite):
    """One complete photo: reserve, upload both roles, commit the paired records.

    Idempotency keys are scoped to this run: re-seeding an account must not collide
    with the receipts a previous run already committed.
    """
    generation = str(uuid.uuid4())
    descriptors = []
    for role, data, field in (
        ("original", original, "resOriginalRes"),
        ("thumbnail", thumbnail, "resJPEGThumbRes"),
    ):
        request = {
            "zoneID": {"zoneName": ZONE},
            "tokens": [
                {
                    "recordName": f"seed-master-{index}",
                    "recordType": "CPLMaster",
                    "fieldName": field,
                    "_syncserver": {
                        "role": role,
                        "expectedSize": len(data),
                        "intent": "create",
                    },
                }
            ],
        }
        headers = {
            "X-Sync-Lease-ID": lease["leaseId"],
            "X-Sync-Lease-Token": lease["leaseToken"],
            "X-Sync-Lease-Fence": lease["fencingToken"],
            "Idempotency-Key": f"seed-{run}-reserve-{index}-{role}",
        }
        reserved = signed_call(
            base,
            f"/database/1/{CONTAINER}/production/private/assets/upload",
            token,
            headers,
            json.dumps(request).encode(),
            content_type="application/json",
        )
        signed = reserved["tokens"][0]["url"]
        parsed = urllib.parse.urlsplit(signed)
        published = signed_call(
            base,
            f"{parsed.path}?{parsed.query}",
            token,
            headers,
            data,
            content_type="application/octet-stream",
        )
        descriptors.append(published["singleFile"])

    create = {
        "zoneID": {"zoneName": ZONE},
        "atomic": True,
        "operations": [
            {
                "operationType": "create",
                "record": {
                    "recordName": f"seed-master-{index}",
                    "recordType": "CPLMaster",
                    "fields": {
                        "photoId": {"type": "STRING", "value": f"seed-photo-{index}"},
                        "contentGeneration": {"type": "STRING", "value": generation},
                        "thumbnailSourceOriginalSHA256": {
                            "type": "STRING",
                            "value": hashlib.sha256(original).hexdigest(),
                        },
                        "resOriginalRes": {"type": "ASSETID", "value": descriptors[0]},
                        "resJPEGThumbRes": {"type": "ASSETID", "value": descriptors[1]},
                    },
                },
            },
            {
                "operationType": "create",
                "record": {
                    "recordName": f"seed-photo-{index}",
                    "recordType": "CPLAsset",
                    "fields": {
                        "masterRef": {
                            "type": "REFERENCE",
                            "value": {"recordName": f"seed-master-{index}", "action": "NONE"},
                        },
                        "contentGeneration": {"type": "STRING", "value": generation},
                        "relativePath": {"type": "STRING", "value": path},
                        "assetDate": {"type": "TIMESTAMP", "value": taken_ms},
                        "isFavorite": {"type": "INT64", "value": 1 if favorite else 0},
                        "isHidden": {"type": "INT64", "value": 0},
                    },
                },
            },
        ],
    }
    call(
        base,
        f"/database/1/{CONTAINER}/production/private/records/modify",
        token,
        body=create,
        headers={
            "X-Sync-Lease-ID": lease["leaseId"],
            "X-Sync-Lease-Token": lease["leaseToken"],
            "X-Sync-Lease-Fence": lease["fencingToken"],
            "Idempotency-Key": f"seed-{run}-create-{index}",
        },
    )


def signed_call(base, path, token, headers, data, content_type=None):
    request = urllib.request.Request(
        base.rstrip("/") + path, data=data, method="POST"
    )
    request.add_header("Accept", "application/json")
    request.add_header("Authorization", f"Bearer {token}")
    for key, value in headers.items():
        request.add_header(key, value)
    if content_type:
        request.add_header("Content-Type", content_type)
    try:
        with urllib.request.urlopen(request) as response:
            payload = response.read()
            return json.loads(payload) if payload else None
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        raise SystemExit(f"POST {path.split('?')[0]} failed: {error.code} {detail}") from None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("base", nargs="?", default="http://127.0.0.1:8790")
    parser.add_argument("--username", default="demo")
    parser.add_argument("--password", default="demo-password-1234")
    parser.add_argument("--count", type=int, default=18)
    args = parser.parse_args()

    token = login(args.base, args.username, args.password)

    snapshot = call(args.base, "/v1/snapshots", token, body={"zoneID": {"zoneName": ZONE}})
    lease = call(
        args.base,
        "/v1/upload-leases",
        token,
        body={
            "deviceId": "seed-script",
            "zoneIDs": [{"zoneName": ZONE}],
        },
    )
    # Every lease control call is fenced by the same three headers, exactly as the
    # client protocol requires.
    lease_headers = {
        "X-Sync-Lease-ID": lease["leaseId"],
        "X-Sync-Lease-Token": lease["leaseToken"],
        "X-Sync-Lease-Fence": lease["fencingToken"],
    }
    call(
        args.base,
        f"/v1/upload-leases/{lease['leaseId']}/check",
        token,
        body={
            "zoneTokens": [{"zoneID": {"zoneName": ZONE}, "syncToken": snapshot["syncToken"]}],
            "localReady": True,
        },
        headers=lease_headers,
    )
    for phase in ("modify", "create"):
        call(
            args.base,
            f"/v1/upload-leases/{lease['leaseId']}/phase",
            token,
            body={"phase": phase},
            headers=lease_headers,
        )

    # A spread of albums, months and capture times so the gallery, the album list and
    # the timeline all have real shape.
    albums = ["Camera/2026-09", "Camera/2026-10", "Camera/2026-10", "Screenshots", "Travel"]
    base_time = int(time.time() * 1000)
    day = 86_400_000
    run = uuid.uuid4().hex[:8]

    try:
        for index in range(args.count):
            album = albums[index % len(albums)]
            seed = (index * 37) % 251
            taken = base_time - (index * 3 + (index // 4)) * day + (index % 5) * 3_600_000
            upload_photo(
                args.base,
                token,
                lease,
                run,
                index,
                png(1200 + seed % 400, 900 + seed % 300, seed),
                png(320, 240, seed),
                f"{album}/IMG_{index + 1000:04d}.png",
                taken,
                favorite=index % 7 == 0,
            )
            print(f"  seeded {index + 1}/{args.count}")
    finally:
        # A lease left behind would block every other client for its whole TTL, so it
        # is released even when seeding failed part way through.
        call(
            args.base,
            f"/v1/upload-leases/{lease['leaseId']}/release",
            token,
            body={},
            headers=lease_headers,
        )

    print(f"done: {args.count} photos for {args.username}")


if __name__ == "__main__":
    sys.exit(main())
