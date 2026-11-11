"""Sequential C-free feature matrix; resumable by code/dependency identity.

For each crate: no features, each C-free feature alone, and all of them together. The
power set (405 cases for each target, 64 of them the 3.3 GiB serde unit) was not run:
this host cannot afford it. `impl-serde` is checked alone, without the API schema.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path('/root/grammers')
SCRATCH = Path('/tmp/claude-0/de27')
TARGETS = ['x86_64-pc-windows-msvc', 'x86_64-apple-darwin', 'aarch64-apple-darwin']
AXES = {
    'grammers': [], 'grammers-crypto': [], 'grammers-mtproto': [],
    'grammers-tl-gen': [], 'grammers-tl-parser': [],
    'grammers-client': ['dispatcher', 'fs', 'html', 'markdown', 'parse_invite_link',
                        'qr-login', 'session-import', 'session-string'],
    'grammers-session': ['serde', 'session-import', 'session-string'],
    'grammers-mtsender': ['hickory-resolver', 'tokio-socks', 'url'],
    'grammers-tl-types': ['deserializable-functions', 'impl-debug', 'impl-from-enum',
                          'impl-from-type', 'impl-serde', 'tl-api', 'tl-mtproto'],
}
DEPS = {
    'grammers-tl-parser': [],
    'grammers-tl-gen': ['grammers-tl-parser'],
    'grammers-tl-types': ['grammers-tl-gen', 'grammers-tl-parser'],
    'grammers-crypto': [],
    'grammers-mtproto': ['grammers-crypto', 'grammers-tl-types'],
    'grammers-session': ['grammers-tl-types'],
    'grammers-mtsender': ['grammers-mtproto', 'grammers-session', 'grammers-crypto', 'grammers-tl-types'],
    'grammers-client': ['grammers-mtsender', 'grammers-session', 'grammers-crypto', 'grammers-tl-types'],
    'grammers': ['grammers-client'],
}

def closure(package):
    result = {package}
    for dependency in DEPS[package]:
        result.update(closure(dependency))
    return result

def source_paths():
    paths = subprocess.check_output(
        ['git', 'ls-files', '-z', '*.rs', '*.tl', '*Cargo.toml', 'Cargo.lock', 'rust-toolchain*', '.cargo/*'],
        cwd=ROOT,
    ).decode().split('\0')
    return list(filter(None, paths))

def fingerprints(context, paths):
    result = {}
    for package in AXES:
        packages = closure(package)
        digest = hashlib.sha256(context.encode())
        for path in sorted(paths):
            if '/' not in path or path.split('/')[0] in packages or path.startswith('.cargo/'):
                digest.update(path.encode() + b'\0' + (ROOT/path).read_bytes() + b'\0')
        result[package] = digest.hexdigest()
    return result

def feature_sets(package):
    """No features, each alone, and all together (for the TL types, all but serde)."""
    axes = sorted(AXES[package])
    together = [f for f in axes if not (package == 'grammers-tl-types' and f == 'impl-serde')]
    sets = [()] + [(feature,) for feature in axes]
    if len(together) > 1:
        sets.append(tuple(together))
    return sets

def snapshot(paths):
    return {str(path): (path.stat().st_size, path.stat().st_mtime_ns, path.stat().st_ctime_ns)
            for path in paths}

def atomic_json(path, value):
    temporary = path.with_suffix(path.suffix+'.tmp')
    with temporary.open('w') as output:
        output.write(json.dumps(value, indent=2)+'\n')
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)

def validate_axes(excluded, env=None):
    metadata = json.loads(subprocess.check_output([
        '/root/.cargo/bin/cargo', 'metadata', '--no-deps', '--locked', '--offline',
        '--format-version', '1',
    ], cwd=ROOT, env=env, text=True))
    packages = {package['name']: package for package in metadata['packages']}
    for package, axes in AXES.items():
        # Cargo includes implicit optional-dependency features in this mapping.
        actual = set(packages[package]['features'])-{'default'}-set(excluded.get(package, []))
        if set(axes)!=actual:
            raise ValueError((package, axes, sorted(actual)))

def valid_logs(record):
    return bool(record.get('log_sha256')) and all(
        Path(path).is_file() and hashlib.sha256(Path(path).read_bytes()).hexdigest()==digest
        for path, digest in record['log_sha256'].items()
    )

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target', action='append', choices=TARGETS)
    parser.add_argument('--package', action='append', choices=AXES)
    parser.add_argument('--retry-resource', action='store_true')
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    logs = args.output/'logs'; logs.mkdir(exist_ok=True)
    env = dict(os.environ, PATH='/root/.cargo/bin:'+os.environ['PATH'],
               CARGO_TARGET_DIR=str(SCRATCH/'target-cross'), CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0', CARGO_TERM_COLOR='never', RUSTFLAGS='-D warnings',
               GUARD_VLIMIT='4000000', GUARD_NICE='19', GUARD_MIN_AVAIL='700',
               GUARD_MIN_AVAIL_SECS='2', GUARD_MIN_SWAPFREE='1200',
               GUARD_MAX_SWAP_GROWTH='300', GUARD_START_AVAIL='1500')
    for name in list(env):
        if name in {'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_BOOTSTRAP'} or name.startswith('CARGO_PROFILE_'):
            env.pop(name)
    env.update(CARGO_PROFILE_DEV_DEBUG='0', RUSTC='/root/.cargo/bin/rustc',
               RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='')
    compiler = subprocess.check_output(['rustc', '-Vv'], env=env, text=True)
    cargo = subprocess.check_output(['cargo', '-Vv'], env=env, text=True)
    runner = Path(__file__).resolve()
    guard = SCRATCH/'guard.sh'
    context = dict(compiler=compiler, cargo=cargo,
                   effective_build_env={name: env[name] for name in
                       ['RUSTFLAGS', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                        'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG']},
                   runner_sha256=hashlib.sha256(runner.read_bytes()).hexdigest(),
                   guard_sha256=hashlib.sha256(guard.read_bytes()).hexdigest())
    paths = source_paths()
    identities = fingerprints(json.dumps(context, sort_keys=True), paths)
    watched = [ROOT/path for path in paths]+[runner, guard]
    frozen = snapshot(watched)
    receipt = args.output/'results.json'
    records = json.loads(receipt.read_text()) if receipt.exists() else []
    seen = {r['key']: r for r in records}
    excluded = {
            'grammers-client': ['proxy', 'session-file', 'session-import-files'],
            'grammers-session': ['sqlite-storage', 'session-import-files'],
            'grammers-mtsender': ['proxy'],
        }
    validate_axes(excluded, env)
    atomic_json(args.output/'configuration.json', {
        **context, 'code_sha256': identities, 'feature_axes': AXES,
        'excluded_C_features': excluded,
        'scope': 'libraries (grammers binary); for each crate no features, each C-free feature alone and all together (TL types: all but impl-serde); default aliases represented by equivalent explicit features. No execution on foreign OS.',
        'flags': ['--locked', '--offline', '-j1', '--no-default-features', 'RUSTFLAGS=-D warnings'],
    })
    cases = []
    for target in args.target or TARGETS:
        for package in sorted(args.package or AXES):
            for flags in feature_sets(package):
                cases.append((target, package, flags))
    for index, (target, package, flags) in enumerate(cases, 1):
        key = json.dumps([target, package, flags, identities[package]], separators=(',', ':'))
        if snapshot(watched)!=frozen or source_paths()!=paths:
            raise SystemExit('Sources/runner/watchdog changed during the matrix; resume with fresh identities.')
        previous = seen.get(key)
        if previous and valid_logs(previous) and (previous['status']=='pass' or
                         previous['status']=='resource-blocked' and not args.retry_resource):
            continue
        feature_hash = hashlib.sha256(','.join(flags).encode()).hexdigest()[:12]
        attempt = sum(r['key']==key for r in records)+1
        label = 'cross-'+target+'-'+package+'-'+feature_hash+'-'+identities[package][:8]+'-'+str(attempt)
        argv = ['cargo', 'check', '--locked', '--offline', '-j', '1', '-p', package,
                '--bins' if package=='grammers' else '--lib', '--target', target, '--no-default-features']
        if flags:
            argv += ['--features', ','.join(flags)]
        case_env = dict(env, GUARD_LOGDIR=str(logs.resolve()))
        if package=='grammers-tl-types' and {'impl-serde', 'tl-api'} <= set(flags):
            case_env.update(GUARD_VLIMIT='none', GUARD_NICE='19', GUARD_MIN_AVAIL='300',
                            GUARD_MIN_AVAIL_SECS='8', GUARD_MIN_SWAPFREE='1200', GUARD_MAX_SWAP_GROWTH='2000')
        started = time.time()
        summary_path = logs/(label+'.sum')
        with summary_path.open('w') as output:
            result = subprocess.run([str(SCRATCH/'guard.sh'), label, '/usr/bin/time', '-v', *argv],
                                    cwd=ROOT, env=case_env, stdout=output, stderr=subprocess.STDOUT)
        summary = summary_path.read_text()
        status = ('deferred' if result.returncode==90 else
                  'resource-blocked' if 'STOPPED_EARLY' in summary else
                  'pass' if not result.returncode else 'failed')
        if snapshot(watched)!=frozen or source_paths()!=paths:
            status = 'source-changed'
        record = dict(key=key, target=target, package=package, features=flags, argv=argv,
                      code_sha256=identities[package], status=status, exit=result.returncode,
                      seconds=round(time.time()-started, 2), summary=str(summary_path),
                      guard_env={k:v for k,v in case_env.items() if k.startswith('GUARD_')},
                      log_sha256={str(path):hashlib.sha256(path.read_bytes()).hexdigest()
                                  for path in [summary_path, logs/(label+'.log')] if path.exists()})
        # Retain earlier attempts instead of overwriting their receipt.
        records.append(record); seen[key]=record
        atomic_json(receipt, records)
        print(index, '/', len(cases), target, package, ','.join(flags) or '(none)', status, flush=True)
        if status in {'failed', 'deferred', 'source-changed'}:
            print(summary[-8000:], flush=True)
            raise SystemExit(result.returncode or 91)
    if snapshot(watched)!=frozen or source_paths()!=paths:
        raise SystemExit('Sources/runner/watchdog changed before matrix completion.')
    print('Matrix traversal finished; inspect resource-blocked rows separately.', flush=True)

if __name__ == '__main__':
    main()
