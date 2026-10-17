#!/usr/bin/env python3
"""Check documentation structure and synthetic contract fixtures, not a server."""
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit
import argparse
import base64
import copy
import hashlib
import json
import re
import struct
import sys
import uuid
import zlib

ROOT = Path(__file__).resolve().parents[1]

def require(condition, message):
    if not condition:
        raise ValueError(message)

def field(record, name, kind):
    value = record['fields'][name]
    require(value['type'] == kind, f'wrong field type: {name}')
    return value['value']

def asset(value):
    uuid.UUID(value['assetId'])
    require(type(value['size']) is int and value['size'] > 0, 'invalid asset size')
    require(value['checksumAlgorithm'] == 'SHA256', 'invalid digest algorithm')
    digest = base64.b64decode(value['fileChecksum'], validate=True)
    require(len(digest) == 32, 'wrong digest size')
    require(base64.b64encode(digest).decode() == value['fileChecksum'], 'noncanonical checksum')
    url = urlsplit(value['downloadURL'])
    require(url.scheme == 'https' and url.hostname == 'sync.example.invalid', 'unsafe fixture URL')
    require(url.path == f"/v1/assets/{value['assetId']}/content", 'asset URL mismatch')
    require(parse_qs(url.query)['role'][0] in ('original', 'thumbnail'), 'invalid URL role')
    require(bool(value['receipt']), 'missing receipt')
    return digest.hex()

def photo(pair):
    master, item = pair['master'], pair['asset']
    require(master['recordType'] == 'CPLMaster' and item['recordType'] == 'CPLAsset', 'record types')
    require(master['zoneID'] == item['zoneID'], 'cross-zone pair')
    require(field(master, 'photoId', 'STRING') == item['recordName'], 'photo identity')
    ref = field(item, 'masterRef', 'REFERENCE')
    require(ref['recordName'] == master['recordName'] and ref['action'] == 'NONE', 'dangling reference')
    if 'zoneID' in ref:
        require(ref['zoneID'] == master['zoneID'], 'cross-zone reference')
    generation = field(master, 'contentGeneration', 'STRING')
    uuid.UUID(generation)
    require(generation == field(item, 'contentGeneration', 'STRING'), 'mixed generation')
    original = field(master, 'resOriginalRes', 'ASSETID')
    thumbnail = field(master, 'resJPEGThumbRes', 'ASSETID')
    original_sha = asset(original)
    asset(thumbnail)
    require(field(master, 'thumbnailSourceOriginalSHA256', 'STRING') == original_sha, 'wrong source digest')
    require(thumbnail['size'] <= 2 * 1024 * 1024, 'thumbnail too large')
    require(thumbnail['mimeType'] in ('image/jpeg', 'image/png'), 'thumbnail MIME')
    for key in ('width', 'height'):
        require(type(thumbnail[key]) is int and 0 < thumbnail[key] <= 2048, 'thumbnail dimensions')
    path = field(item, 'relativePath', 'STRING')
    require(isinstance(path, str) and path and not path.startswith('/') and '\\' not in path and '\0' not in path, 'unsafe path')
    require(all(part not in ('', '.', '..') for part in path.split('/')), 'unsafe path segment')
    for name in ('isFavorite', 'isHidden'):
        value = field(item, name, 'INT64')
        require(type(value) is int and value in (0, 1), 'invalid boolean field')

def check_fixtures():
    fixtures = json.loads((ROOT/'docs/contracts/fixtures.json').read_text())
    binaries = fixtures['binaryAssets']
    require({x['role'] for x in binaries} == {'original', 'thumbnail'}, 'missing binary roles')
    for entry in binaries:
        data = base64.b64decode(entry['bytesBase64'], validate=True)
        value = entry['value']
        require(len(data) == value['size'], 'fixture size mismatch')
        require(parse_qs(urlsplit(value['downloadURL']).query)['role'] == [entry['role']], 'response role mismatch')
        digest = hashlib.sha256(data).hexdigest()
        require(digest == entry['sha256'] == asset(value), 'fixture digest mismatch')
        require(data[:8] == b'\x89PNG\r\n\x1a\n', 'PNG signature')
        width, height = struct.unpack('>II', data[16:24])
        require((width, height) == (value['width'], value['height']), 'PNG dimensions')
        position = 8
        while position < len(data):
            size = struct.unpack('>I', data[position:position+4])[0]
            tag = data[position+4:position+8]
            payload = data[position+8:position+8+size]
            crc = struct.unpack('>I', data[position+8+size:position+12+size])[0]
            require(zlib.crc32(tag+payload)&0xffffffff == crc, 'PNG CRC')
            position += 12+size
        require(position == len(data), 'PNG length')
    pair = fixtures['photoPair']
    photo(pair)
    create = fixtures['createPhotoRequest']
    require(create['atomic'] is True, 'photo create must be atomic')
    require([op['record'] for op in create['operations']] == [pair['master'], pair['asset']], 'create fixture drift')
    require(all(op['operationType'] == 'create' for op in create['operations']), 'create operations')
    delete = fixtures['deletePhotoRequest']
    require(delete['atomic'] is True and len(delete['operations']) == 2, 'delete must be paired atomic')
    require({op['record']['recordName'] for op in delete['operations']} == {pair['master']['recordName'], pair['asset']['recordName']}, 'delete identity')
    require(all(op['operationType'] == 'delete' and op['record']['recordChangeTag'] for op in delete['operations']), 'delete conditions')
    token = fixtures['reserveUploadRequest']['tokens'][0]
    thumb = next(x for x in binaries if x['role'] == 'thumbnail')
    require(token['fieldName'] == 'resJPEGThumbRes' and token['_syncserver']['role'] == 'thumbnail', 'role field')
    require(token['_syncserver']['expectedSize'] == thumb['value']['size'], 'upload size')
    require(token['_syncserver']['expectedSha256'] == thumb['sha256'], 'upload digest')
    require(fixtures['publishedUploadResponse']['singleFile'] == thumb['value'], 'upload result')
    require(fixtures['uploadStatusResponse']['asset'] == thumb['value'] and fixtures['uploadStatusResponse']['state'] == 'Published', 'upload status')
    require(fixtures['snapshotResponse']['snapshotToken'] != fixtures['snapshotResponse']['syncToken'], 'distinct token kinds')
    mutations = [
        lambda p: p['asset']['fields']['contentGeneration'].update(value='22222222-2222-4222-8222-222222222222'),
        lambda p: p['asset']['fields']['masterRef']['value'].update(recordName='missing'),
        lambda p: p['asset']['fields']['relativePath'].update(value='../escape.png'),
        lambda p: p['master']['fields'].pop('resJPEGThumbRes'),
        lambda p: p['master']['fields']['resOriginalRes']['value'].update(checksumAlgorithm='SHA1'),
        lambda p: p['master']['fields']['resJPEGThumbRes']['value'].update(size=True),
        lambda p: p['master']['fields']['thumbnailSourceOriginalSHA256'].update(value='0'*64),
    ]
    for mutate in mutations:
        invalid = copy.deepcopy(pair)
        mutate(invalid)
        try:
            photo(invalid)
        except (ValueError, KeyError):
            continue
        raise ValueError('Malformed contract fixture unexpectedly accepted')
    return len(binaries), len(mutations)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docs-only", action="store_true", help="also reject Rust implementation during M0")
    args = parser.parse_args()
    links = examples = 0
    docs = sorted(path for path in ROOT.rglob('*.md')
                  if not set(path.relative_to(ROOT).parts)
                  & {'target', '.git', 'local-run', 'node_modules', 'dist'})
    for path in docs:
        text = path.read_text(encoding='utf-8')
        for target in re.findall(r'\[[^\]]*\]\(([^\n)]+)\)', text):
            target = target.strip().strip('<>')
            parsed = urlsplit(target)
            if parsed.scheme or target.startswith('#'):
                continue
            resolved = (path.parent/unquote(parsed.path)).resolve()
            require(resolved.is_relative_to(ROOT) and resolved.exists(), f'{path.relative_to(ROOT)}: invalid local link {target}')
            links += 1
        require(len(re.findall(r'^```', text, flags=re.M)) % 2 == 0, f'{path}: fences')
        for block in re.findall(r'^```json\s*\n(.*?)^```\s*$', text, flags=re.M|re.S):
            json.loads(block)
            examples += 1
    required = ['README.md','AGENTS.md','Agent.md','spac.md','LICENSE','docs/design/readiness-review.md','docs/design/client-publication.md','docs/contracts/snapshots.md','docs/contracts/photo-records.md','docs/contracts/assets.md','docs/contracts/fixtures.json','docs/adr/0003-freeze-bootstrap-photo-and-asset-contracts.md']
    for name in required:
        require((ROOT/name).is_file(), f'missing {name}')
    if args.docs_only:
        require(not list(ROOT.rglob('*.rs')) and not (ROOT/'Cargo.toml').exists(), 'docs-stage Rust scope')
    binaries, negative = check_fixtures()
    print(f'PASS: {len(docs)} Markdown documents, {links} local links, {examples} JSON examples; {binaries} synthetic PNG/hash fixtures, {negative} malformed-pair checks; no server execution')

if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, json.JSONDecodeError) as error:
        print(f'FAIL: {error}', file=sys.stderr)
        sys.exit(1)
