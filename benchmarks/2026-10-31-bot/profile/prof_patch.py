import sys
p=sys.argv[1]+'/grammers-client/tests/bot_workload.rs'
s=open(p).read()
s=s.replace('''        let mut seen = HashSet::new();
        for symbol in flat {
            let name = name_of(symbol);
            if seen.insert(name.clone()) {
                *inclusive.entry(name).or_default() += *count;
            }
        }''','''        let mut seen = HashSet::new();
        for symbol in flat {
            let loc = loc_of(symbol);
            let name = format!("{} @ {}", name_of(symbol), loc);
            if loc.starts_with("grammers-") && seen.insert(name.clone()) {
                *inclusive.entry(name).or_default() += *count;
            }
        }
        // Leaf category, then what the whole stack was doing.
        if let Some(top) = flat.first() {
            let name = top.name();
            let loc = loc_of(top);
            let leaf = if ["malloc","cfree","realloc","_int_","calloc","memalign","posix_memalign","__rust_alloc","__rust_dealloc","__rust_realloc","sysmalloc","tcache","arena_get","__libc_free","free"].iter().any(|m| name.contains(m)) { "allocator" }
                else if ["__send","recv","epoll_wait","syscall","getrandom","__write","__read"].iter().any(|m| name == *m || name.starts_with(m)) || (name == "write" && loc.ends_with(":0")) { "kernel" }
                else if name == "clock_gettime" { "clock" }
                else if ["memcpy","memmove","memset"].iter().any(|m| name.contains(m)) || ["ptr/mod.rs","result.rs","boxed.rs"].iter().any(|m| loc.contains(m)) { "large copies" }
                else if loc.contains("aes") || loc.contains("sha") || loc.contains("block_api") || loc.contains("sse2") || name.contains("_mm_") { "crypto" }
                else { "other" };
            *categories.entry(leaf.to_string()).or_default() += *count;
            for (needle, label) in [("deserialize", "inclusive: TL decoding"), ("to_bytes", "inclusive: TL encoding"), ("process_updates", "inclusive: message box"), ("peer_map_from_raw", "inclusive: peer map"), ("cache_peer_map", "inclusive: peer storage"), ("from_raw", "inclusive: friendly update"), ("decrypt_data_v2", "inclusive: decrypt"), ("encrypt_data_v2", "inclusive: encrypt"), ("send_message", "inclusive: send_message"), ("on_net_read", "inclusive: on_net_read"), ("try_fill_write", "inclusive: try_fill_write")] {
                if flat.iter().any(|symbol| symbol.name().contains(needle)) {
                    *categories.entry(label.to_string()).or_default() += *count;
                }
            }
        }''')
s=s.replace('''    let mut self_lines: HashMap<String, isize> = HashMap::new();''','''    let mut self_lines: HashMap<String, isize> = HashMap::new();
    let mut categories: HashMap<String, isize> = HashMap::new();''')
s=s.replace('print("self", self_time, 45);','print("categories", categories, 30);')
s=s.replace('print("self by line", self_lines, 25);','print("self by line", self_lines, 45);')
s=s.replace('print("inclusive", inclusive, 70);','print("inclusive by grammers line", inclusive, 40);')
open(p,'w').write(s)
