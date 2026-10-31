#!/usr/bin/env python3
"""Create a scratch copy of a grammers tree whose bot_workload test also samples CPU with pprof."""
import shutil, subprocess, sys, pathlib, re
src, dst = map(pathlib.Path, sys.argv[1:3])
if dst.exists():
    shutil.rmtree(dst)
subprocess.check_call(['rsync','-rlpt','--no-devices','--no-specials','--delete','--exclude=target','--exclude=.git','--exclude=node_modules','--exclude=benchmarks','--exclude=docs',f'{src}/',f'{dst}/'])
p = dst/'grammers-client/tests/bot_workload.rs'
s = p.read_text()
marker = """    let started = Instant::now();
    tokio::time::sleep(config.window).await;
    let elapsed = started.elapsed();
"""
assert marker in s
s = s.replace(marker, """    let profiler = pprof::ProfilerGuardBuilder::default().frequency(1999).build().unwrap();
    let started = Instant::now();
    tokio::time::sleep(config.window).await;
    let elapsed = started.elapsed();
    let report = profiler.report().build().unwrap();
    drop(profiler);
    print_cpu_report(&report);
""")
s += r'''
fn print_cpu_report(report: &pprof::Report) {
    use std::collections::{HashMap, HashSet};
    let mut total = 0isize;
    let mut self_time: HashMap<String, isize> = HashMap::new();
    let mut inclusive: HashMap<String, isize> = HashMap::new();
    let mut self_lines: HashMap<String, isize> = HashMap::new();
    let name_of = |symbol: &pprof::Symbol| -> String {
        let raw = symbol.name();
        let mut n = regex_lite_strip(&raw);
        n.truncate(110);
        n
    };
    let loc_of = |symbol: &pprof::Symbol| -> String {
        let file = symbol.filename.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let file = file.rsplit_once("/grammers-").map(|(_, r)| format!("grammers-{r}")).unwrap_or_else(|| file.rsplit('/').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/"));
        format!("{}:{}", file, symbol.lineno.unwrap_or(0))
    };
    for (frames, count) in &report.data {
        total += *count;
        let flat: Vec<&pprof::Symbol> = frames.frames.iter().flatten().collect();
        // Skip profiler/signal frames at the top of each stack.
        let start = flat.iter().position(|s| { let n = s.name(); !(n.contains("pprof") || n.contains("backtrace") || n.contains("perf_signal_handler") || n.contains("__restore_rt") || n.contains("sigaction") || n.contains("<unknown>")) }).unwrap_or(0);
        let flat = &flat[start..];
        if let Some(top) = flat.first() {
            *self_time.entry(name_of(top)).or_default() += *count;
            *self_lines.entry(format!("{} @ {}", name_of(top), loc_of(top))).or_default() += *count;
        }
        let mut seen = HashSet::new();
        for symbol in flat {
            let name = name_of(symbol);
            if seen.insert(name.clone()) {
                *inclusive.entry(name).or_default() += *count;
            }
        }
    }
    println!("CPU samples: {total}");
    let print = |title: &str, map: HashMap<String, isize>, n: usize| {
        let mut rows: Vec<_> = map.into_iter().collect();
        rows.sort_by_key(|(_, c)| -*c);
        println!("--- {title}");
        for (name, count) in rows.into_iter().take(n) {
            println!("{:6.2}% {}", count as f64 * 100.0 / total as f64, name);
        }
    };
    print("self", self_time, 45);
    print("self by line", self_lines, 25);
    print("inclusive", inclusive, 70);
}

fn regex_lite_strip(name: &str) -> String {
    // Drop generic arguments and crate hash suffixes for grouping.
    let mut out = String::new();
    let mut depth = 0i32;
    for c in name.chars() {
        match c {
            '<' => { depth += 1; if depth == 1 && out.ends_with("::") { out.truncate(out.len()-2);} if depth == 1 && out.is_empty() { out.push('<'); depth = 0; } }
            '>' => { depth = (depth - 1).max(0); }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    match out.rfind("::h") {
        Some(at) if out.len() - at == 19 => out.truncate(at),
        _ => {}
    }
    out
}
'''
p.write_text(s)
c = dst/'grammers-client/Cargo.toml'
t = c.read_text()
t = t.replace('[target.\'cfg(target_os = "linux")\'.dev-dependencies]\nlibc = "0.2"\n','[target.\'cfg(target_os = "linux")\'.dev-dependencies]\nlibc = "0.2"\npprof = { version = "0.15", default-features = false, features = ["cpp"] }\n')
c.write_text(t)
