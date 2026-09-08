#!/usr/bin/env python3
"""Measure playlist/segment latency on an existing local smoke video.

Restart only rust_stream before running to measure its cold cache. The gateway
and network may still have warm caches; results are not public-network timings.
"""
import argparse
import json
import statistics
import time
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', default='http://127.0.0.1:18080')
parser.add_argument('--samples', type=int, default=10)
args = parser.parse_args()
base = args.url.rstrip('/')
if urllib.parse.urlsplit(base).hostname not in ('localhost', '127.0.0.1', '::1'):
    parser.error('Use the loopback local test stack')
if not 1 <= args.samples <= 1000:
    parser.error('--samples must be 1..1000')
with urllib.request.urlopen(base + '/api/videos', timeout=60) as response:
    video = json.load(response)[0]
path = '/stream/' + video['id'] + '/360p/playlist.m3u8'


def fetch(path):
    start = time.perf_counter()
    with urllib.request.urlopen(base + path, timeout=60) as response:
        data = response.read(32 * 1024 * 1024 + 1)
    if len(data) > 32 * 1024 * 1024:
        raise ValueError('Unexpectedly large segment')
    return data, time.perf_counter() - start


playlist, playlist_time = fetch(path)
segment = next(x for x in playlist.decode().splitlines() if x and not x.startswith('#'))
if not segment.startswith('/'):
    segment = path.rsplit('/', 1)[0] + '/' + segment
_, first = fetch(segment)
warm = [fetch(segment)[1] for _ in range(args.samples)]
print(json.dumps({'video_id': video['id'], 'playlist_seconds': playlist_time,
                  'first_segment_seconds': first, 'warm_segment_seconds': warm,
                  'warm_median_seconds': statistics.median(warm)}, indent=2))
