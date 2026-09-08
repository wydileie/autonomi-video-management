#!/usr/bin/env python3
"""Repeatable local-devnet payment, streaming, and duplicate-request benchmark.

Uses the funded local test chain only. Never accepts a public network health response.
Set ANTD_INTERNAL_TOKEN; run --bytes 1073745920 for a genuine multi-batch original.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import tempfile
import time
import urllib.parse
import uuid


def run(args):
    base = urllib.parse.urlsplit(args.url)
    if base.scheme != 'http' or base.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('Benchmark requires an explicit loopback local-devnet gateway')
    token = os.environ['ANTD_INTERNAL_TOKEN']
    timings = {}

    def request(method, path, payload=None, file=None, extra=None, stream=False):
        connection = http.client.HTTPConnection(base.hostname, base.port or 8082, timeout=3600)
        headers = {'Authorization': f'Bearer {token}'}
        headers.update(extra or {})
        body = None
        if file is not None:
            body = file.open('rb')
            headers['Content-Length'] = str(file.stat().st_size)
            headers['Content-Type'] = 'application/octet-stream'
        elif payload is not None:
            body = json.dumps(payload, separators=(',', ':')).encode()
            headers['Content-Type'] = 'application/json'
        try:
            connection.request(method, path, body, headers)
            response = connection.getresponse()
            if response.status != 200:
                raise RuntimeError(f'{path}: HTTP {response.status} {response.read(65536).decode()}')
            if stream:
                digest = hashlib.sha256()
                size = 0
                while chunk := response.read(1024 * 1024):
                    digest.update(chunk)
                    size += len(chunk)
                return size, digest.hexdigest()
            return json.loads(response.read(32 * 1024 * 1024))
        finally:
            if file is not None:
                body.close()
            connection.close()

    health = request('GET', '/health')
    if health.get('network') != 'local' or not health.get('write_ready') or health.get('protocol_version') != 'autvid-gateway-v2':
        raise RuntimeError('Application gateway must report the funded local network and write readiness')
    with tempfile.TemporaryDirectory(prefix='autvid-benchmark-') as directory:
        source = Path(directory) / 'original.bin'
        digest = hashlib.sha256()
        with source.open('wb') as out:
            remaining = args.bytes
            while remaining:
                chunk = os.urandom(min(1024 * 1024, remaining))
                out.write(chunk)
                digest.update(chunk)
                remaining -= len(chunk)
        sha = digest.hexdigest()
        started = time.monotonic()
        quote = request('POST', '/v1/file/cost?payment_mode=merkle', file=source)
        timings['quote_seconds'] = time.monotonic() - started
        assert quote['file_size'] == args.bytes and quote['content_sha256'] == sha
        contents = [{'sha256': sha, 'byte_size': args.bytes, 'payment_mode': quote['payment_mode']}]
        quote_id = str(uuid.uuid4())
        approval = {
            'quote_id': quote_id,
            'network': quote['network'],
            'content_digest': hashlib.sha256(json.dumps([quote['network'], contents], separators=(',', ':')).encode()).hexdigest(),
            'expires_at': int(time.time()) + 3600,
            'max_storage_atto': quote['cost'],
            'max_gas_wei': quote['estimated_gas_cost_wei'],
            'contents': contents,
        }
        request('POST', '/v1/payments/approvals', approval)
        job = str(uuid.uuid4())
        request('POST', '/v1/payments/lease', {'quote_id': quote_id, 'job_id': job, 'owner': 'benchmark', 'generation': 1, 'expires_at': int(time.time()) + 3600})
        headers = {'x-payment-approval': quote_id, 'x-payment-lease': f'{job}:benchmark:1', 'x-content-sha256': sha}
        started = time.monotonic()
        receipt = request('POST', '/v1/file/public?payment_mode=merkle&verify=true', file=source, extra=headers)
        timings['upload_and_verify_seconds'] = time.monotonic() - started
        assert receipt['address'] == quote['address'] and receipt['verified']
        assert receipt['chunks_failed'] == 0 and receipt['chunks_stored'] == receipt['total_chunks']
        before = request('GET', f"/v1/payments/uploads/{receipt['upload_id']}")
        for name in ['first_download_seconds', 'repeat_download_seconds']:
            started = time.monotonic()
            assert request('GET', f"/v1/file/public/{receipt['address']}/raw", stream=True) == (args.bytes, sha)
            timings[name] = time.monotonic() - started
        started = time.monotonic()
        repeated = request('POST', '/v1/file/public?payment_mode=merkle&verify=true', file=source, extra=headers)
        timings['duplicate_request_seconds'] = time.monotonic() - started
        assert repeated == receipt
        after = request('GET', f"/v1/payments/uploads/{receipt['upload_id']}")
        assert after['transaction_count'] == before['transaction_count']
        if args.bytes > 1024**3:
            assert after['transaction_count'] >= 3, 'Expected approval plus multiple Merkle payments'
        return {'bytes': args.bytes, 'gateway_health': health, 'timings': timings,
                'receipt': receipt, 'transaction_count': after['transaction_count'],
                'retry_additional_transactions': 0}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:8082')
    parser.add_argument('--bytes', type=int, default=20 * 1024 * 1024)
    arguments = parser.parse_args()
    if arguments.bytes < 3:
        parser.error('--bytes must be at least 3')
    print(json.dumps(run(arguments), indent=2))
