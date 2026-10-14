#!/usr/bin/env python3
"""Read-only checks; never put an API key on the command line or log tokens."""
import argparse
import ipaddress
import json
import pathlib
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("base_url")
parser.add_argument("--api-key-file", type=pathlib.Path, required=True)
args = parser.parse_args()
base = urllib.parse.urlsplit(args.base_url)
try:
    loopback = ipaddress.ip_address(base.hostname or "").is_loopback
except ValueError:
    loopback = False
if base.username or base.password or base.query or base.fragment or base.path not in ("", "/"):
    raise SystemExit("Use a credential-free root base URL")
if base.scheme != "https" and not (base.scheme == "http" and loopback):
    raise SystemExit("Use HTTPS outside literal loopback")
key = args.api_key_file.read_text().strip()

def call(path, body=None, authenticated=True):
    headers = {"Authorization": "Bearer " + key} if authenticated else {}
    data = None if body is None else json.dumps(body).encode()
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(args.base_url.rstrip("/") + path, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=10) as response:
        assert response.headers["Cache-Control"] == "no-store"
        return json.load(response)

assert call("/healthz", authenticated=False)["status"] == "alive"
assert call("/readyz", authenticated=False)["status"] == "ready"
caps = call("/v1/capabilities")
for name in ["recordCrud", "snapshots", "incrementalChanges", "uploadLeases", "assetUpload", "assetDownload", "photoPublication"]:
    assert caps["capabilities"][name], name
container = urllib.parse.quote(caps["namespace"]["container"], safe="")
ck = f"/database/1/{container}/production/private"
zones = call(ck + "/zones/list", {})["zones"]
for zone in zones:
    snapshot = call("/v1/snapshots", {"zoneID": zone["zoneID"]})
    delta = call(ck + "/changes/zone", {"zones": [{"zoneID": zone["zoneID"], "syncToken": snapshot["syncToken"]}]})["zones"][0]
    assert "serverErrorCode" not in delta
    assert isinstance(delta["moreComing"], bool)
print(f"PASS: health, readiness, capabilities, {len(zones)} zone snapshots and delta reads")
