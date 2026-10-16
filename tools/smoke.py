#!/usr/bin/env python3
"""Read-only checks; never put a password or API key on the command line or log tokens."""
import argparse
import getpass
import ipaddress
import json
import pathlib
import sys
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("base_url")
auth = parser.add_mutually_exclusive_group(required=True)
auth.add_argument("--api-key-file", type=pathlib.Path, help="legacy single-account deployments")
auth.add_argument("--username", help="account deployment; password is prompted or read from stdin")
parser.add_argument("--password-stdin", action="store_true", help="read the password from stdin instead of prompting")
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
def login(username):
    if args.password_stdin:
        password = sys.stdin.readline().rstrip("\n")
    elif sys.stdin.isatty():
        password = getpass.getpass(f"Password for {username}: ")
    else:
        raise SystemExit("Use --password-stdin when stdin is not a terminal")
    body = json.dumps({"username": username, "password": password}).encode()
    request = urllib.request.Request(args.base_url.rstrip("/") + "/v1/auth/login", data=body,
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)["accessToken"]

key = args.api_key_file.read_text().strip() if args.api_key_file else login(args.username)

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
for name in ["recordCrud", "snapshots", "incrementalChanges", "uploadLeases", "assetUpload", "assetDownload", "photoPublication", "operationalMetrics"]:
    assert caps["capabilities"][name], name
container = urllib.parse.quote(caps["namespace"]["container"], safe="")
ck = f"/database/1/{container}/production/private"
zones = call(ck + "/zones/list", {})["zones"]
for zone in zones:
    snapshot = call("/v1/snapshots", {"zoneID": zone["zoneID"]})
    delta = call(ck + "/changes/zone", {"zones": [{"zoneID": zone["zoneID"], "syncToken": snapshot["syncToken"]}]})["zones"][0]
    assert "serverErrorCode" not in delta
    assert isinstance(delta["moreComing"], bool)
diagnostic = call("/v1/metrics")
assert diagnostic["version"] == 1 and diagnostic["ready"] is True
assert diagnostic["runId"] == caps["recovery"]["runId"]
assert diagnostic["metrics"]["database"]["queueCapacity"] == caps["limits"]["dbQueueCapacity"]
assert diagnostic["metrics"]["database"]["controlQueueCapacity"] == caps["limits"]["controlQueueCapacity"]
assert diagnostic["metrics"]["zoneChangePages"] >= len(zones)
print(f"PASS: health, readiness, capabilities, operational metrics, {len(zones)} zone snapshots and delta reads")
