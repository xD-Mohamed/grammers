#!/usr/bin/env python3
"""Type-check `grammers-tl-types` with `impl-serde` in parts, on a host too small for one pass.

With `impl-serde` every generated type derives `Serialize` and `Deserialize`: 3,351 derives
at layer 233 with every feature. One compiler process needs more than 2.8 GB for them.
This makes a scratch copy of the crate whose build script keeps the derive on one part of
the items and gives every other item a stub implementation, so that each part is expanded
and type-checked exactly as generated, against the whole type graph, in a fraction of the
memory. The parts are checked one at a time.

    python3 serde_parts.py <grammers tree> <scratch dir> [--parts 4] [--features ...]

Each part runs `cargo check -j 1` with incremental compilation and debuginfo off, under a
4 GB address-space limit, and is stopped if the machine's available memory falls under
`--min-available-mb`. Nothing in the tree is changed. This does not replace a single
build of the feature where one fits: the stubs are not what a user compiles.
"""
import argparse
import os
import pathlib
import re
import resource
import shutil
import subprocess
import sys
import threading
import time

PARTITION = r'''
/// Scratch only: keep the serde derive on one part of the items and give every other
/// item an unimplemented stub, so that each part type-checks against the whole graph.
fn partition(out: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=TLPART_N");
    println!("cargo:rerun-if-env-changed=TLPART_K");
    let n: usize = env::var("TLPART_N").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let k: isize = env::var("TLPART_K").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    if n == 0 {
        return Ok(());
    }
    const MARK: &str = "#[derive(serde_derive::Serialize, serde_derive::Deserialize)]";
    let mut idx = 0usize;
    let mut kept = 0usize;
    for f in ["generated_types.rs", "generated_enums.rs", "generated_functions.rs"] {
        let path = out.join(f);
        let text = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = text.lines().collect();
        let mut res = String::with_capacity(text.len());
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.trim() != MARK {
                res.push_str(line);
                res.push('\n');
                i += 1;
                continue;
            }
            let keep = (idx % n) as isize == k;
            idx += 1;
            if keep {
                kept += 1;
                res.push_str(line);
                res.push('\n');
                i += 1;
                continue;
            }
            let indent = &line[..line.len() - line.trim_start().len()];
            let mut j = i + 1;
            loop {
                let t = lines[j].trim_start();
                if t.starts_with("pub struct ") || t.starts_with("pub enum ") {
                    break;
                }
                j += 1;
            }
            let decl = lines[j].trim_start();
            assert!(decl.ends_with('{'), "unexpected declaration: {decl}");
            let rest = decl.splitn(3, ' ').nth(2).unwrap();
            let name: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let generic = rest[name.len()..].starts_with("<X>");
            assert!(generic || rest[name.len()..].starts_with(" {"), "unexpected: {decl}");
            let (gs, gd, ty) = if generic {
                ("<X: serde::Serialize>", "<'de, X: serde::Deserialize<'de>>", format!("{name}<X>"))
            } else {
                ("", "<'de>", name.clone())
            };
            for l in &lines[i + 1..=j] {
                res.push_str(l);
                res.push('\n');
            }
            let close = format!("{indent}}}");
            let mut m = j + 1;
            loop {
                let l = lines[m];
                if l == close {
                    res.push_str(l);
                    res.push('\n');
                    res.push_str(&format!(
                        "{indent}impl{gs} serde::Serialize for {ty} {{ fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {{ unimplemented!() }} }}\n"
                    ));
                    res.push_str(&format!(
                        "{indent}impl{gd} serde::Deserialize<'de> for {ty} {{ fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {{ unimplemented!() }} }}\n"
                    ));
                    break;
                }
                if !l.trim_start().starts_with("#[serde(") {
                    res.push_str(l);
                    res.push('\n');
                }
                m += 1;
            }
            i = m + 1;
        }
        std::fs::write(&path, res)?;
    }
    println!("cargo:warning=serde parts: {kept} of {idx} derives kept (part {k} of {n})");
    Ok(())
}
'''

MANIFEST = '''[package]
name = "grammers-tl-types"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
serde = { version = "~1.0", optional = true }
serde_derive = { version = "~1.0", optional = true }
serde_bytes = { version = "0.11.19", optional = true }

[build-dependencies]
grammers-tl-gen = { path = "TREE/grammers-tl-gen" }
grammers-tl-parser = { path = "TREE/grammers-tl-parser" }

[features]
default = ["impl-debug", "impl-from-enum", "impl-from-type", "tl-api"]
deserializable-functions = []
impl-debug = []
impl-from-enum = []
impl-from-type = []
impl-serde = ["dep:serde", "dep:serde_derive", "dep:serde_bytes"]
tl-api = []
tl-mtproto = []
'''


def available_mb():
    for line in open('/proc/meminfo'):
        if line.startswith('MemAvailable'):
            return int(line.split()[1]) // 1024
    return 0


def prepare(tree, scratch):
    crate = tree / 'grammers-tl-types'
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    shutil.copytree(crate / 'src', scratch / 'src')
    shutil.copytree(crate / 'tl', scratch / 'tl')
    build = (crate / 'build.rs').read_text()
    tail = '    outputs.flush()?;\n\n    Ok(())\n}'
    if tail not in build:
        sys.exit('build.rs no longer ends the way this tool expects')
    build = build.replace(tail, '    outputs.flush()?;\n    drop(outputs);\n    partition(&output_dir)?;\n\n    Ok(())\n}')
    (scratch / 'build.rs').write_text(build + PARTITION)
    features = re.search(r'\[features\](.*?)(\n\[|\Z)', (crate / 'Cargo.toml').read_text(), re.S).group(1)
    expected = re.search(r'\[features\](.*)', MANIFEST, re.S).group(1)
    if sorted(features.split()) != sorted(expected.split()):
        sys.exit('the crate\'s features changed: update MANIFEST in this tool')
    (scratch / 'Cargo.toml').write_text(MANIFEST.replace('TREE', str(tree)))


def run_part(scratch, target, parts, part, features, min_available):
    env = dict(os.environ, TLPART_N=str(parts), TLPART_K=str(part), CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0')
    command = ['nice', '-n', '10', 'cargo', 'check', '-j', '1', '--target-dir', str(target)] + features
    limit = 4_000_000 * 1024
    process = subprocess.Popen(command, cwd=scratch, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True,
                               preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_AS, (limit, limit)))
    lowest = [available_mb()]
    stopped = []

    def watch():
        while process.poll() is None:
            now = available_mb()
            lowest[0] = min(lowest[0], now)
            if now < min_available:
                stopped.append(now)
                subprocess.run(['pkill', '-KILL', '-P', str(process.pid)])
                process.kill()
                return
            time.sleep(1)

    watcher = threading.Thread(target=watch)
    watcher.start()
    output = process.communicate()[0]
    watcher.join()
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss // 1024
    kept = re.search(r'serde parts: (\d+) of (\d+)', output)
    print(f'part {part} of {parts}: exit {process.returncode}, derives {kept.group(1) if kept else "?"}'
          f' of {kept.group(2) if kept else "?"}, largest process so far {peak} MB,'
          f' lowest available memory {lowest[0]} MB'
          + (f', STOPPED at {stopped[0]} MB available' if stopped else ''), flush=True)
    if process.returncode:
        print(output[-3000:])
    return process.returncode == 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('tree', type=pathlib.Path)
    parser.add_argument('scratch', type=pathlib.Path)
    parser.add_argument('--parts', type=int, default=4)
    parser.add_argument('--features', nargs='*', default=['--all-features'])
    parser.add_argument('--min-available-mb', type=int, default=700)
    parser.add_argument('--start-available-mb', type=int, default=1500)
    args = parser.parse_args()
    tree = args.tree.resolve()
    scratch = args.scratch.resolve()
    if available_mb() < args.start_available_mb:
        sys.exit(f'only {available_mb()} MB available: not starting')
    prepare(tree, scratch / 'crate')
    ok = all(run_part(scratch / 'crate', scratch / 'target', args.parts, part, args.features,
                      args.min_available_mb) for part in range(args.parts))
    print('all parts type-check' if ok else 'FAILED')
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
