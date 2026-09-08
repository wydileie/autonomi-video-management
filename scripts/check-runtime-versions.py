#!/usr/bin/env python3
"""Check reviewed runtime pins against build inputs and both committed lockfiles."""
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
pins = json.loads((ROOT / 'deploy/versions.json').read_text())
errors = []


def require(path, text):
    if text not in (ROOT / path).read_text():
        errors.append(f'{path}: expected {text!r}')


def lock_versions(path, expected):
    packages = tomllib.loads((ROOT / path).read_text())['package']
    for name, version in expected.items():
        versions = {p['version'] for p in packages if p['name'] == name}
        if version not in versions:
            errors.append(f'{path}: {name} requires {version}; found {sorted(versions)}')


lock_versions('Cargo.lock', {k: v for k, v in pins['autonomi'].items() if k not in ('node', 'sdk', 'cli')})
lock_versions('apps/desktop/src-tauri/Cargo.lock', pins['desktop'])
require('rust-toolchain.toml', f'channel = "{pins["rust"]}"')
require('.nvmrc', pins['node'])
for path in ('crates/antd_service/Dockerfile', 'crates/rust_admin/Dockerfile', 'crates/rust_stream/Dockerfile', 'deploy/autonomi_devnet/Dockerfile'):
    require(path, f'rustup toolchain install {pins["rust"]}')
require('.devcontainer/Dockerfile', f'--default-toolchain {pins["rust"]}')
require('.devcontainer/Dockerfile', f'nodejs={pins["node"]}-1nodesource1')
require('apps/web/Dockerfile', f'FROM node:{pins["node"]}-')
for path in ('.github/workflows/ci.yml', '.github/workflows/desktop-release.yml'):
    content = (ROOT / path).read_text()
    if set(re.findall(r'dtolnay/rust-toolchain@([^\s]+)', content)) != {pins['rust']}:
        errors.append(f'{path}: Rust toolchain drift')
    if set(re.findall(r'node-version: "([^"]+)"', content)) != {pins['node']}:
        errors.append(f'{path}: Node toolchain drift')
for path in ('.devcontainer/Dockerfile', 'deploy/autonomi_devnet/Dockerfile'):
    require(path, f'AUTONOMI_ANT_NODE_REF=v{pins["autonomi"]["node"]}')
require('.devcontainer/Dockerfile', f'AUTONOMI_ANT_SDK_REF=v{pins["autonomi"]["sdk"]}')
require('.devcontainer/Dockerfile', f'AUTONOMI_ANT_CLIENT_REF=ant-cli-v{pins["autonomi"]["cli"]}')
for name, value in pins['images'].items():
    overlay = 'logging' if name in ('LOKI_IMAGE', 'ALLOY_IMAGE') else 'monitoring'
    require(f'deploy/docker-compose.{overlay}.yml', f'{name}:-{value}')
    # Examples may omit an image override; every supplied override must match.
    for path in ROOT.glob('.env*.example'):
        match = re.search(rf'^{name}=(.*)$', path.read_text(), re.M)
        if match and match.group(1).strip('"\'') != value:
            errors.append(f'{path.name}: {name} differs from reviewed pin')
require('crates/antd_service/src/routes/health.rs', pins['gateway_protocol'])
require('crates/rust_admin/src/antd_client.rs', pins['gateway_protocol'])
if errors:
    print('\n'.join(errors), file=sys.stderr)
    sys.exit(1)
print('Runtime pins and committed lockfiles agree.')
