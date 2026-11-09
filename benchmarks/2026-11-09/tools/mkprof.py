#!/usr/bin/env python3
"""Scratch copy of a grammers tree whose loopback and bot_workload harnesses sample CPU with pprof.

Usage: mkprof.py <source tree> <scratch dir>
Set LOOPBACK_PROFILE=<file> / BOT_PROFILE=<file> when running to write folded stacks
(`thread;root;...;leaf count`). Measurement tool only; never part of the library.
"""
import pathlib
import shutil
import subprocess
import sys

src, dst = map(pathlib.Path, sys.argv[1:3])
if dst.exists():
    shutil.rmtree(dst)
subprocess.check_call(['rsync', '-rlpt', '--no-devices', '--no-specials', '--delete', '--exclude=target',
                       '--exclude=.git', '--exclude=node_modules', '--exclude=benchmarks', '--exclude=docs',
                       f'{src}/', f'{dst}/'])

HELPER = r'''
#[allow(dead_code)]
mod cpu_profile {
    use std::io::Write;
    pub fn start(var: &str) -> Option<(String, pprof::ProfilerGuard<'static>)> {
        let path = std::env::var(var).ok()?;
        let hz = std::env::var("PROFILE_HZ").map_or(1999, |v| v.parse().unwrap());
        Some((path, pprof::ProfilerGuardBuilder::default().frequency(hz).build().unwrap()))
    }
    pub fn finish(guard: Option<(String, pprof::ProfilerGuard<'static>)>) {
        let Some((path, guard)) = guard else { return };
        let report = guard.report().build().unwrap();
        drop(guard);
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut total = 0isize;
        for (frames, count) in &report.data {
            total += *count;
            let mut line = frames.thread_name.replace([';', ' '], "_");
            // pprof lists the innermost frame first; fold root first.
            for (depth, symbols) in frames.frames.iter().enumerate().rev() {
                for (inline, symbol) in symbols.iter().enumerate().rev() {
                    line.push(';');
                    line.push_str(&symbol.name().replace([';', ' '], "_"));
                    if depth == 0 && inline == 0 || symbol.filename.as_ref().is_some_and(|f| f.to_string_lossy().contains("grammers")) {
                        let file = symbol.filename.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                        let file = file.rsplit('/').take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
                        line.push_str(&format!("@{}:{}", file, symbol.lineno.unwrap_or(0)));
                    }
                }
            }
            writeln!(out, "{line} {count}").unwrap();
        }
        println!("CPU profile: {total} samples written to {path}");
    }
}
'''

def patch(path, edits, dep_file, dep_anchor):
    text = path.read_text()
    for old, new in edits:
        assert old in text, (path, old)
        text = text.replace(old, new, 1)
    path.write_text(text + HELPER)
    manifest = dep_file.read_text()
    assert dep_anchor in manifest, dep_file
    dep_file.write_text(manifest.replace(
        dep_anchor, dep_anchor + 'pprof = { version = "0.15", default-features = false, features = ["cpp"] }\n', 1))

patch(dst / 'grammers-mtsender/tests/loopback.rs', [
    ("                TRACK.store(track, Relaxed);\n                let cpu = thread_cpu();\n",
     "                TRACK.store(track, Relaxed);\n                let profile = cpu_profile::start(\"LOOPBACK_PROFILE\");\n                let cpu = thread_cpu();\n"),
    ("                let spent = thread_cpu();\n                let elapsed = started.elapsed();\n                TRACK.store(false, Relaxed);\n",
     "                let spent = thread_cpu();\n                let elapsed = started.elapsed();\n                TRACK.store(false, Relaxed);\n                cpu_profile::finish(profile);\n"),
], dst / 'grammers-mtsender/Cargo.toml', "[target.'cfg(target_os = \"linux\")'.dev-dependencies]\nlibc = \"0.2\"\n")

bot = dst / 'grammers-client/tests/bot_workload.rs'
marker = "    let started = Instant::now();\n    tokio::time::sleep(config.window).await;\n    let elapsed = started.elapsed();\n"
if bot.exists() and marker in bot.read_text():
    patch(bot, [(marker, "    let profile = cpu_profile::start(\"BOT_PROFILE\");\n" + marker + "    cpu_profile::finish(profile);\n")],
          dst / 'grammers-client/Cargo.toml', "[target.'cfg(target_os = \"linux\")'.dev-dependencies]\nlibc = \"0.2\"\n")
print('scratch profile tree at', dst)
