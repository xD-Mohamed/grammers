//! Local model of a busy bot: one long-lived session, many concurrent small RPCs and a
//! stream of live updates, against a mock in a **separate process**, so process CPU,
//! heap and RSS below are the client's alone. No Telegram endpoint, account or
//! credential is used. Every case is ignored:
//!
//! ```sh
//! cargo test --locked --release -p grammers-client --features session-file --test bot_workload -- \
//!     --exact bot_workload --ignored --nocapture --test-threads=1
//! ```
//!
//! Configured by `BOT_*` variables, see [`Config`]. A closed loop finds the client's
//! capacity; an open loop models a fixed request rate, where each request costs its own
//! wakeups and system calls. Either way this measures local client efficiency only.
//! Telegram's server-side rate limits are unrelated to it and cannot be raised by a
//! client library.

#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader as StdBufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddrV4, SocketAddrV6};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use grammers_client::Client;
use grammers_crypto::aes::{ige_decrypt, ige_encrypt};
use grammers_mtsender::{ConnectionParams, SenderPool, UpdatesConfiguration};
use grammers_session::Session;
use grammers_session::SessionData;
use grammers_session::storages::MemorySession;
use grammers_session::types::{PeerAuth, PeerId, PeerInfo, PeerRef};
use grammers_tl_types::{self as tl, Deserializable, Identifiable, Serializable};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Allocation accounting, switched on only for the tracked window, which follows the CPU
// and latency window: otherwise it would cost each allocation more than the allocator's
// own work and flatter whichever build allocates less.

struct Counting;

static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

fn note_alloc(size: usize) {
    if TRACK.load(Relaxed) {
        let index = ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Relaxed);
        let every = SITE_EVERY.load(Relaxed);
        if every != 0 && index.is_multiple_of(every) {
            record_site(size, every);
        }
    }
}

// Optional allocation-site attribution (`BOT_SITE_EVERY`), which is far too slow to
// share a run with the CPU or latency figures: every Nth allocation is charged to its
// call stack. Sizes are only known per sample, so bytes are estimates.
static SITE_EVERY: AtomicU64 = AtomicU64::new(0);
static SITES: Mutex<Option<HashMap<String, (u64, u64)>>> = Mutex::new(None);

thread_local! {
    static IN_SITE: Cell<bool> = const { Cell::new(false) };
}

fn record_site(size: usize, every: u64) {
    if IN_SITE.with(|flag| flag.replace(true)) {
        return;
    }
    let trace = format!("{}", std::backtrace::Backtrace::force_capture());
    let mut frames = Vec::new();
    let mut lines = trace.lines().peekable();
    while let Some(line) = lines.next() {
        let name = line
            .trim_start()
            .split_once(": ")
            .map_or("", |(_, name)| name);
        let location = lines.next_if(|next| next.trim_start().starts_with("at "));
        let keep = [
            "grammers",
            "tokio::sync",
            "tokio::task",
            "tokio::runtime::task",
            "bytes::",
            "futures_util",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix) || name.contains(&format!("<{prefix}")));
        if keep && !name.contains("bot_workload") && !name.contains("note_alloc") {
            let file = location.map_or("", |line| line.trim_start().trim_start_matches("at "));
            let file = file.rsplit_once("/grammers").map_or(file, |(_, path)| path);
            frames.push(format!("{name} @ {file}"));
        }
    }
    let key = if frames.is_empty() {
        "<runtime or harness>".to_string()
    } else {
        frames.truncate(6);
        frames.join("\n    ")
    };
    let mut sites = SITES.lock().unwrap();
    let entry = sites
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .or_default();
    entry.0 += every;
    entry.1 += every * size as u64;
    drop(sites);
    IN_SITE.with(|flag| flag.set(false));
}

fn report_sites(rpcs: f64) {
    let Some(sites) = SITES.lock().unwrap().take() else {
        return;
    };
    let mut rows: Vec<_> = sites.into_iter().collect();
    rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    let total: u64 = rows.iter().map(|(_, (_, bytes))| bytes).sum();
    let count: u64 = rows.iter().map(|(_, (count, _))| count).sum();
    println!("ALLOC SITES by bytes ({count} sampled-equivalent allocations over {rpcs:.0} RPCs)");
    for (site, (allocations, bytes)) in rows.iter().take(25) {
        println!(
            "{:>6.2}% bytes {:>9.0} B/rpc {:>6.2} allocs/rpc\n    {site}",
            *bytes as f64 * 100.0 / total as f64,
            *bytes as f64 / rpcs,
            *allocations as f64 / rpcs,
        );
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            note_alloc(new_size);
        }
        new
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ---------------------------------------------------------------------------
// Configuration, process measurements and a fixed-size latency histogram.

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T
where
    T::Err: std::fmt::Debug,
{
    std::env::var(name).map_or(default, |value| value.parse().unwrap())
}

struct Config {
    /// `BOT_RUNTIME`: `current`, or `multi` with `BOT_WORKERS` workers.
    multi: bool,
    workers: usize,
    /// `BOT_CONCURRENCY`: closed-loop callers, or the in-flight limit of an open loop.
    concurrency: usize,
    /// `BOT_RATE`: requests per second; 0 selects the closed loop.
    rate: u64,
    /// `BOT_WARM_MS`, `BOT_WINDOW_MS`, `BOT_TRACK_MS`: warm-up, CPU and heap windows.
    warm: Duration,
    window: Duration,
    track: Duration,
}

impl Config {
    fn load() -> Self {
        Self {
            multi: env_or::<String>("BOT_RUNTIME", "current".into()) == "multi",
            workers: env_or("BOT_WORKERS", 2),
            concurrency: env_or("BOT_CONCURRENCY", 64),
            rate: env_or("BOT_RATE", 0),
            warm: Duration::from_millis(env_or("BOT_WARM_MS", 1000)),
            window: Duration::from_millis(env_or("BOT_WINDOW_MS", 3000)),
            track: Duration::from_millis(env_or("BOT_TRACK_MS", 1000)),
        }
    }
}

fn pin_thread(variable: &str) {
    if let Ok(cpus) = std::env::var(variable) {
        let mut set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
        for cpu in cpus.split(',').map(|cpu| cpu.parse::<usize>().unwrap()) {
            assert!(cpu < libc::CPU_SETSIZE as usize);
            unsafe { libc::CPU_SET(cpu, &mut set) };
        }
        // SAFETY: valid initialized affinity set; pid 0 selects the calling thread.
        assert_eq!(
            unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) },
            0
        );
    }
}

/// CPU spent by every thread of this process, which holds no mock work.
fn process_cpu() -> (Duration, Duration) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the valid out-pointer on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let duration =
        |v: libc::timeval| Duration::from_micros(v.tv_sec as u64 * 1_000_000 + v.tv_usec as u64);
    (duration(usage.ru_utime), duration(usage.ru_stime))
}

fn context_switches() -> (i64, i64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    (usage.ru_nvcsw, usage.ru_nivcsw)
}

fn proc_status_kib(field: &str) -> u64 {
    // VmRSS is asynchronously batched by the kernel, distorting small-process
    // comparisons. Read page tables for the steady snapshot instead.
    // This runs after the CPU window and before allocation accounting begins.
    let (path, field) = if field == "VmRSS" {
        ("/proc/self/smaps_rollup", "Rss:")
    } else {
        // VmHWM remains the kernel's approximate high-water counter.
        ("/proc/self/status", field)
    };
    let status = std::fs::read_to_string(path).unwrap();
    let line = status.lines().find(|line| line.starts_with(field)).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// Log-linear buckets: 64 per power of two, about 1.1% resolution, no allocation.
const LATENCY_BUCKETS: usize = 64 * 40;
static LATENCY: [AtomicU64; LATENCY_BUCKETS] = [const { AtomicU64::new(0) }; LATENCY_BUCKETS];

fn latency_bucket(nanos: u64) -> usize {
    let nanos = nanos.max(64);
    let power = 63 - nanos.leading_zeros() as usize;
    let fraction = ((nanos >> (power - 6)) & 63) as usize;
    (((power - 6) * 64) + fraction).min(LATENCY_BUCKETS - 1)
}

fn bucket_nanos(bucket: usize) -> u64 {
    let power = bucket / 64 + 6;
    let fraction = (bucket % 64) as u64;
    (64 + fraction) << (power - 6)
}

fn reset_latency() {
    LATENCY.iter().for_each(|bucket| bucket.store(0, Relaxed));
}

fn latency_percentile(percent: f64) -> f64 {
    let total: u64 = LATENCY.iter().map(|bucket| bucket.load(Relaxed)).sum();
    let target = (total as f64 * percent / 100.0).ceil() as u64;
    let mut seen = 0;
    for (bucket, count) in LATENCY.iter().enumerate() {
        seen += count.load(Relaxed);
        if seen >= target.max(1) {
            return bucket_nanos(bucket) as f64 / 1000.0;
        }
    }
    0.0
}

// ---------------------------------------------------------------------------
// The mock server, in its own process. It implements only what these measurements
// need: one MTProto 2.0 packet per reply, plus one pushed update per `UPDATE_EVERY`.

const API_ID: i32 = 1;
const HOME_DC: i32 = 2;
const MSG_CONTAINER: u32 = 0x73f1_f8dc;
const RPC_RESULT: u32 = 0xf35c_6d01;
const BOOL_TRUE: u32 = 0x9972_75b5;
const INVOKE_WITH_LAYER: u32 =
    tl::functions::InvokeWithLayer::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID;
const SELF_ID: i64 = 777_000;
const OTHER_ID: i64 = 123_456;
const OTHER_HASH: i64 = 0x1234_5678;
const CHANNEL_ID: i64 = 987_654;
const CHANNEL_HASH: i64 = 0x7654_3210;

fn auth_key() -> [u8; 256] {
    let mut key = [0; 256];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    key
}

struct ServerKey {
    data: [u8; 256],
    id: [u8; 8],
}

impl ServerKey {
    fn new(data: [u8; 256]) -> Self {
        let sha = Sha1::digest(data);
        Self {
            data,
            id: sha[12..20].try_into().unwrap(),
        }
    }

    /// MTProto 2.0 key derivation; `x` is 0 from the client and 8 to it.
    fn derive(&self, msg_key: &[u8; 16], x: usize) -> ([u8; 32], [u8; 32]) {
        let mut a = Sha256::new();
        a.update(msg_key);
        a.update(&self.data[x..x + 36]);
        let a = a.finalize();
        let mut b = Sha256::new();
        b.update(&self.data[40 + x..76 + x]);
        b.update(msg_key);
        let b = b.finalize();
        let mut key = [0; 32];
        key[..8].copy_from_slice(&a[..8]);
        key[8..24].copy_from_slice(&b[8..24]);
        key[24..].copy_from_slice(&a[24..]);
        let mut iv = [0; 32];
        iv[..8].copy_from_slice(&b[..8]);
        iv[8..24].copy_from_slice(&a[8..24]);
        iv[24..].copy_from_slice(&b[24..]);
        (key, iv)
    }

    fn decrypt<'a>(&self, packet: &'a mut [u8]) -> &'a [u8] {
        assert_eq!(packet[..8], self.id, "unexpected auth key id");
        let msg_key: [u8; 16] = packet[8..24].try_into().unwrap();
        let (key, iv) = self.derive(&msg_key, 0);
        ige_decrypt(&mut packet[24..], &key, &iv);
        &packet[24..]
    }

    /// Append one abridged frame carrying `plain` encrypted for the client.
    fn encrypt_frame(&self, mut plain: Vec<u8>, out: &mut Vec<u8>) {
        let padding = 12 + (16 - (plain.len() + 12) % 16) % 16;
        plain.extend(std::iter::repeat_n(0x5a, padding));
        let mut large = Sha256::new();
        large.update(&self.data[96..128]);
        large.update(&plain);
        let msg_key: [u8; 16] = large.finalize()[8..24].try_into().unwrap();
        let (key, iv) = self.derive(&msg_key, 8);
        ige_encrypt(&mut plain, &key, &iv);
        let words = (24 + plain.len()) / 4;
        if words < 127 {
            out.push(words as u8);
        } else {
            out.extend_from_slice(&(0x7f | ((words as u32) << 8)).to_le_bytes());
        }
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&msg_key);
        out.extend_from_slice(&plain);
    }
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn envelope(bytes: &[u8], at: usize) -> (i64, &[u8], usize) {
    let msg_id = i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let len = read_u32(bytes, at + 12) as usize;
    (msg_id, &bytes[at + 16..at + 16 + len], at + 16 + len)
}

fn messages(plain: &[u8]) -> Vec<(i64, &[u8])> {
    let (msg_id, body, _) = envelope(plain, 16); // after salt and session_id
    if read_u32(body, 0) != MSG_CONTAINER {
        return vec![(msg_id, body)];
    }
    let mut at = 8;
    (0..read_u32(body, 4))
        .map(|_| {
            let (msg_id, inner, next) = envelope(body, at);
            at = next;
            (msg_id, inner)
        })
        .collect()
}

fn user(id: i64, access_hash: i64, is_self: bool) -> tl::enums::User {
    tl::types::User {
        is_self,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: is_self,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(format!("First{id}")),
        last_name: None,
        username: Some(format!("name{id}")),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: is_self.then_some(1),
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
    .into()
}

fn channel() -> tl::enums::Chat {
    tl::types::Channel {
        creator: false,
        left: false,
        broadcast: false,
        verified: false,
        megagroup: true,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        pinned_msg_id: None,
        id: CHANNEL_ID,
        access_hash: Some(CHANNEL_HASH),
        title: "Group".into(),
        username: None,
        photo: tl::enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
    }
    .into()
}

fn config_reply() -> Vec<u8> {
    tl::enums::Config::Config(tl::types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: false,
        blocked_mode: false,
        force_try_ipv6: false,
        date: 1_700_000_000,
        expires: 1_700_003_600,
        test_mode: false,
        this_dc: HOME_DC,
        dc_options: Vec::new(),
        dc_txt_domain_name: String::new(),
        chat_size_max: 200,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 210_000,
        offline_blur_timeout_ms: 5_000,
        offline_idle_timeout_ms: 30_000,
        online_cloud_timeout_ms: 300_000,
        notify_cloud_delay_ms: 30_000,
        notify_default_delay_ms: 1_500,
        push_chat_period_ms: 60_000,
        push_chat_limit: 2,
        edit_time_limit: 172_800,
        revoke_time_limit: i32::MAX,
        revoke_pm_time_limit: i32::MAX,
        rating_e_decay: 2_419_200,
        stickers_recent_limit: 200,
        channels_read_media_period: 604_800,
        tmp_sessions: None,
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: String::new(),
        autoupdate_url_prefix: None,
        gif_search_username: None,
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1_024,
        message_length_max: 4_096,
        webfile_dc_id: 4,
        suggested_lang_code: None,
        lang_pack_version: None,
        base_lang_pack_version: None,
        reactions_default: None,
        autologin_token: None,
    })
    .to_bytes()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A plain text message, as a small group or private chat would produce.
fn message(id: i32, from: i64, peer: tl::enums::Peer, date: i32) -> tl::enums::Message {
    tl::types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: Some(tl::types::PeerUser { user_id: from }.into()),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: "hello there, this is a typical short message".into(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    }
    .into()
}

struct Connection {
    pts: i32,
    channel_pts: i32,
    message_id: i32,
    replies: u64,
    sequence: u64,
    update_every: u64,
    peers: i64,
    reply_delay: Duration,
}

impl Connection {
    /// Replies are in order on one connection, so `pts` stays free of gaps.
    fn next_msg_id(&mut self) -> i64 {
        let n = self.sequence;
        self.sequence += 1;
        ((now_secs() << 32) | ((n & 0x3fff_ffff) << 2) | 1) as i64
    }

    fn next_pts(&mut self) -> i32 {
        self.pts += 1;
        self.message_id += 1;
        self.pts
    }

    fn reply_to(&mut self, msg_id: i64, body: &[u8]) -> Option<(Vec<u8>, bool)> {
        let constructor = read_u32(body, 0);
        let date = now_secs() as i32;
        match constructor {
            tl::types::MsgsAck::CONSTRUCTOR_ID => None,
            tl::functions::PingDelayDisconnect::CONSTRUCTOR_ID
            | tl::functions::Ping::CONSTRUCTOR_ID => {
                let ping_id = i64::from_le_bytes(body[4..12].try_into().unwrap());
                let pong = tl::enums::Pong::Pong(tl::types::Pong { msg_id, ping_id });
                Some((pong.to_bytes(), false))
            }
            tl::functions::GetFutureSalts::CONSTRUCTOR_ID => {
                let salts = tl::enums::FutureSalts::Salts(tl::types::FutureSalts {
                    req_msg_id: msg_id,
                    now: date,
                    salts: tl::RawVec(
                        (0..64)
                            .map(|i| tl::types::FutureSalt {
                                valid_since: date - 60 + i * 1800,
                                valid_until: date + 1800 + i * 1800,
                                salt: 0,
                            })
                            .collect(),
                    ),
                });
                Some((salts.to_bytes(), false))
            }
            INVOKE_WITH_LAYER => Some((config_reply(), true)),
            tl::functions::updates::GetState::CONSTRUCTOR_ID => {
                let state = tl::enums::updates::State::State(tl::types::updates::State {
                    pts: self.pts,
                    qts: 0,
                    date,
                    seq: 0,
                    unread_count: 0,
                });
                Some((state.to_bytes(), true))
            }
            tl::functions::messages::SendMessage::CONSTRUCTOR_ID => {
                let buffer = &mut tl::Cursor::from_slice(&body[4..]);
                let request = tl::functions::messages::SendMessage::deserialize(buffer).unwrap();
                if matches!(request.peer, tl::enums::InputPeer::Channel(_)) {
                    self.channel_pts += 1;
                    let peer = tl::types::PeerChannel {
                        channel_id: CHANNEL_ID,
                    }
                    .into();
                    let updates = tl::types::Updates {
                        updates: vec![
                            tl::types::UpdateMessageId {
                                id: self.channel_pts,
                                random_id: request.random_id,
                            }
                            .into(),
                            tl::types::UpdateNewChannelMessage {
                                message: message(self.channel_pts, SELF_ID, peer, date),
                                pts: self.channel_pts,
                                pts_count: 1,
                            }
                            .into(),
                        ],
                        users: vec![user(SELF_ID, 1, true)],
                        chats: vec![channel()],
                        date,
                        seq: 0,
                    };
                    return Some((tl::enums::Updates::Updates(updates).to_bytes(), true));
                }
                let pts = self.next_pts();
                let sent =
                    tl::enums::Updates::UpdateShortSentMessage(tl::types::UpdateShortSentMessage {
                        out: true,
                        id: self.message_id,
                        pts,
                        pts_count: 1,
                        date,
                        media: None,
                        entities: None,
                        ttl_period: None,
                    });
                Some((sent.to_bytes(), true))
            }
            // Reached only if the client's own update queue overflowed. A real server would
            // return what was missed; jumping forward is enough to resume here.
            tl::functions::updates::GetDifference::CONSTRUCTOR_ID => {
                let difference = tl::enums::updates::Difference::TooLong(
                    tl::types::updates::DifferenceTooLong { pts: self.pts },
                );
                Some((difference.to_bytes(), true))
            }
            tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID => {
                let difference = tl::enums::updates::ChannelDifference::Empty(
                    tl::types::updates::ChannelDifferenceEmpty {
                        r#final: true,
                        pts: self.channel_pts,
                        timeout: None,
                    },
                );
                Some((difference.to_bytes(), true))
            }
            tl::functions::users::GetUsers::CONSTRUCTOR_ID => {
                let users = vec![user(OTHER_ID, OTHER_HASH, false)];
                Some((users.to_bytes(), true))
            }
            // messages.setTyping and anything else a bot would acknowledge.
            _ => Some((BOOL_TRUE.to_le_bytes().to_vec(), true)),
        }
    }

    /// A message for this bot, alternating a private chat and a large group.
    fn pushed_update(&mut self) -> Vec<u8> {
        let date = now_secs() as i32;
        // `BOT_PEERS` senders in turn, which only matters to peer storage.
        let sender_id = OTHER_ID + (self.replies / self.update_every) as i64 % self.peers;
        let sender = user(sender_id, OTHER_HASH ^ sender_id, false);
        let updates = if self.replies.is_multiple_of(2 * self.update_every) {
            let pts = self.next_pts();
            let peer = tl::types::PeerUser { user_id: OTHER_ID }.into();
            tl::types::Updates {
                updates: vec![
                    tl::types::UpdateNewMessage {
                        message: message(self.message_id, sender_id, peer, date),
                        pts,
                        pts_count: 1,
                    }
                    .into(),
                ],
                users: vec![sender],
                chats: Vec::new(),
                date,
                seq: 0,
            }
        } else {
            self.channel_pts += 1;
            let peer = tl::types::PeerChannel {
                channel_id: CHANNEL_ID,
            }
            .into();
            tl::types::Updates {
                updates: vec![
                    tl::types::UpdateNewChannelMessage {
                        message: message(self.channel_pts, sender_id, peer, date),
                        pts: self.channel_pts,
                        pts_count: 1,
                    }
                    .into(),
                ],
                users: vec![sender],
                chats: vec![channel()],
                date,
                seq: 0,
            }
        };
        tl::enums::Updates::Updates(updates).to_bytes()
    }

    fn frame(
        &mut self,
        key: &ServerKey,
        session_id: &[u8; 8],
        request: i64,
        result: Option<&[u8]>,
        direct: &[u8],
        out: &mut Vec<u8>,
    ) {
        let body_len = result.map_or(direct.len(), |result| 12 + result.len());
        let mut message = Vec::with_capacity(64 + body_len);
        message.extend_from_slice(&[0; 8]); // salt
        message.extend_from_slice(session_id);
        message.extend_from_slice(&self.next_msg_id().to_le_bytes());
        message.extend_from_slice(&1i32.to_le_bytes()); // content-related
        message.extend_from_slice(&(body_len as i32).to_le_bytes());
        if let Some(result) = result {
            message.extend_from_slice(&RPC_RESULT.to_le_bytes());
            message.extend_from_slice(&request.to_le_bytes());
            message.extend_from_slice(result);
        } else {
            message.extend_from_slice(direct);
        }
        key.encrypt_frame(message, out);
    }

    async fn serve(mut self, key: Arc<ServerKey>, stream: TcpStream) {
        stream.set_nodelay(true).unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        if read.read_u8().await.ok() != Some(0xef) {
            return;
        }
        let mut packet = Vec::new();
        let mut out = Vec::new();
        loop {
            let Ok(first) = read.read_u8().await else {
                return;
            };
            let words = if first < 127 {
                usize::from(first)
            } else {
                let mut ext = [0; 3];
                if read.read_exact(&mut ext).await.is_err() {
                    return;
                }
                u32::from_le_bytes([ext[0], ext[1], ext[2], 0]) as usize
            };
            packet.resize(words * 4, 0);
            if read.read_exact(&mut packet).await.is_err() {
                return;
            }
            let plain = key.decrypt(&mut packet);
            let session_id: [u8; 8] = plain[8..16].try_into().unwrap();
            out.clear();
            for (msg_id, body) in messages(plain) {
                let Some((reply, wrapped)) = self.reply_to(msg_id, body) else {
                    continue;
                };
                if wrapped {
                    self.frame(&key, &session_id, msg_id, Some(&reply), &[], &mut out);
                    self.replies += 1;
                    if self.update_every != 0 && self.replies.is_multiple_of(self.update_every) {
                        let update = self.pushed_update();
                        self.frame(&key, &session_id, 0, None, &update, &mut out);
                    }
                } else {
                    self.frame(&key, &session_id, 0, None, &reply, &mut out);
                }
            }
            if !self.reply_delay.is_zero() {
                tokio::time::sleep(self.reply_delay).await;
            }
            if !out.is_empty() && write.write_all(&out).await.is_err() {
                return;
            }
        }
    }
}

/// Runs in the child process: serve until stdin closes.
#[test]
#[ignore = "mock server role of bot_workload"]
fn bot_mock_server() {
    if std::env::var_os("BOT_MOCK_ROLE").is_none() {
        return;
    }
    pin_thread("BOT_SERVER_CPUS");
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    println!("PORT {}", listener.local_addr().unwrap().port());
    std::io::stdout().flush().unwrap();
    std::thread::spawn(|| {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        std::process::exit(0);
    });
    let update_every = env_or("BOT_UPDATE_EVERY", 2);
    let reply_delay = Duration::from_micros(env_or("BOT_REPLY_DELAY_US", 0));
    let peers = env_or("BOT_PEERS", 1i64);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(env_or("BOT_SERVER_THREADS", 2))
        .on_thread_start(|| pin_thread("BOT_SERVER_CPUS"))
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let key = Arc::new(ServerKey::new(auth_key()));
        let listener = TcpListener::from_std(listener).unwrap();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection = Connection {
                pts: 1000,
                channel_pts: 1000,
                message_id: 1000,
                replies: 0,
                sequence: 0,
                update_every,
                peers,
                reply_delay,
            };
            tokio::spawn(connection.serve(Arc::clone(&key), stream));
        }
    });
}

// ---------------------------------------------------------------------------
// The client.

#[derive(Default)]
struct Counters {
    completed: AtomicU64,
    errors: AtomicU64,
    updates: AtomicU64,
    update_errors: AtomicU64,
    dropped: AtomicU64,
}

/// One bot action: half are sent messages, to a private chat, which gets a short reply,
/// and a large group, which gets full updates. The rest are small acknowledged calls.
async fn act(client: &Client, [private, group]: [PeerRef; 2], turn: u64, text: &str) -> bool {
    match turn % 4 {
        0 => client.send_message(private, text).await.is_ok(),
        2 => client.send_message(group, text).await.is_ok(),
        1 => client
            .invoke(&tl::functions::messages::SetTyping {
                peer: private.into(),
                top_msg_id: None,
                action: tl::enums::SendMessageAction::SendMessageTypingAction,
            })
            .await
            .is_ok(),
        _ => client
            .invoke(&tl::functions::users::GetUsers {
                id: vec![
                    tl::types::InputUser {
                        user_id: OTHER_ID,
                        access_hash: OTHER_HASH,
                    }
                    .into(),
                ],
            })
            .await
            .is_ok(),
    }
}

/// `started` is when the request was due, so that a client falling behind an open loop
/// is charged for the wait as well as for the request.
async fn timed_act(
    client: &Client,
    peers: [PeerRef; 2],
    turn: u64,
    counters: &Counters,
    started: Instant,
) {
    if act(
        client,
        peers,
        turn,
        "hello, this is a short reply from a bot",
    )
    .await
    {
        LATENCY[latency_bucket(started.elapsed().as_nanos() as u64)].fetch_add(1, Relaxed);
        counters.completed.fetch_add(1, Relaxed);
    } else {
        counters.errors.fetch_add(1, Relaxed);
    }
}

/// `BOT_SESSION=sqlite` persists to a real file, as a deployed bot would.
async fn run_client(config: Config, port: u16) {
    let sqlite = std::env::var("BOT_SESSION").is_ok_and(|kind| kind == "sqlite");
    #[cfg(not(feature = "session-file"))]
    assert!(!sqlite, "BOT_SESSION=sqlite needs --features session-file");
    #[cfg(feature = "session-file")]
    if sqlite {
        use grammers_session::storages::SqliteSession;
        use grammers_session::types::DcOption;
        let path = std::env::temp_dir().join(format!("bot-workload-{}.db", std::process::id()));
        let session = SqliteSession::open(&path).await.unwrap();
        session.set_home_dc_id(HOME_DC).await.unwrap();
        session
            .set_dc_option(&DcOption {
                id: HOME_DC,
                ipv4: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
                ipv6: SocketAddrV6::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped(), port, 0, 0),
                auth_key: Some(auth_key()),
            })
            .await
            .unwrap();
        run_client_with(config, Arc::new(session)).await;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        return;
    }
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut data = SessionData {
        home_dc: HOME_DC,
        ..SessionData::default()
    };
    let home = data.dc_options.get_mut(&HOME_DC).unwrap();
    home.ipv4 = address;
    home.ipv6 = SocketAddrV6::new(address.ip().to_ipv6_mapped(), port, 0, 0);
    home.auth_key = Some(auth_key());
    run_client_with(config, Arc::new(MemorySession::from(data))).await;
}

/// Runs one workload to completion and prints its one-line measurement.
async fn run_client_with<S>(config: Config, session: Arc<S>)
where
    S: Session,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    session
        .cache_peer(PeerInfo::User {
            id: SELF_ID,
            auth: Some(PeerAuth::from_hash(1)),
            bot: Some(true),
            is_self: Some(true),
        })
        .await
        .unwrap();

    let pool = SenderPool::with_configuration(
        Arc::clone(&session),
        API_ID,
        ConnectionParams {
            ack_batch: env_or("BOT_ACK_BATCH", 1),
            ..Default::default()
        },
    );
    let client = Client::new(pool.handle.clone());
    let SenderPool {
        runner,
        handle,
        updates,
    } = pool;
    tokio::spawn(runner.run());
    let mut stream = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: false })
        .await
        .unwrap();

    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));

    let consumer = {
        let counters = Arc::clone(&counters);
        tokio::spawn(async move {
            loop {
                match stream.next().await {
                    Ok(update) => {
                        std::hint::black_box(&update);
                        counters.updates.fetch_add(1, Relaxed);
                    }
                    Err(_) => {
                        counters.update_errors.fetch_add(1, Relaxed);
                    }
                }
            }
        })
    };

    let peers = [
        PeerRef {
            id: PeerId::user(OTHER_ID).unwrap(),
            auth: PeerAuth::from_hash(OTHER_HASH),
        },
        PeerRef {
            id: PeerId::channel(CHANNEL_ID).unwrap(),
            auth: PeerAuth::from_hash(CHANNEL_HASH),
        },
    ];
    // Establish the connection and the update position before any window.
    client
        .invoke(&tl::functions::Ping { ping_id: 1 })
        .await
        .unwrap();

    let load = if config.rate == 0 {
        let callers: Vec<_> = (0..config.concurrency as u64)
            .map(|caller| {
                let client = client.clone();
                let counters = Arc::clone(&counters);
                let stop = Arc::clone(&stop);
                tokio::spawn(async move {
                    let mut turn = caller;
                    while !stop.load(Relaxed) {
                        timed_act(&client, peers, turn, &counters, Instant::now()).await;
                        turn += 1;
                    }
                })
            })
            .collect();
        tokio::spawn(async move {
            for caller in callers {
                caller.await.unwrap();
            }
        })
    } else {
        let client = client.clone();
        let counters = Arc::clone(&counters);
        let stop = Arc::clone(&stop);
        let (rate, limit) = (config.rate, config.concurrency as u64);
        tokio::spawn(async move {
            let in_flight = Arc::new(AtomicU64::new(0));
            let start = tokio::time::Instant::now();
            let mut turn = 0;
            while !stop.load(Relaxed) {
                let due = start + Duration::from_secs_f64(turn as f64 / rate as f64);
                tokio::time::sleep_until(due).await;
                turn += 1;
                if in_flight.load(Relaxed) >= limit {
                    counters.dropped.fetch_add(1, Relaxed);
                    continue;
                }
                in_flight.fetch_add(1, Relaxed);
                let client = client.clone();
                let counters = Arc::clone(&counters);
                let in_flight = Arc::clone(&in_flight);
                tokio::spawn(async move {
                    timed_act(&client, peers, turn, &counters, due.into_std()).await;
                    in_flight.fetch_sub(1, Relaxed);
                });
            }
        })
    };

    tokio::time::sleep(config.warm).await;

    reset_latency();
    let sample = |counters: &Counters| {
        (
            counters.completed.load(Relaxed),
            counters.updates.load(Relaxed),
            counters.errors.load(Relaxed),
            counters.dropped.load(Relaxed),
        )
    };
    let before = sample(&counters);
    let cpu = process_cpu();
    let switches = context_switches();
    let started = Instant::now();
    tokio::time::sleep(config.window).await;
    let elapsed = started.elapsed();
    let spent = process_cpu();
    let switched = context_switches();
    let after = sample(&counters);
    let (p50, p95, p99) = (
        latency_percentile(50.0),
        latency_percentile(95.0),
        latency_percentile(99.0),
    );
    let rss_steady = proc_status_kib("VmRSS");

    let tracked = sample(&counters);
    let allocations = ALLOCATIONS.load(Relaxed);
    let allocated = ALLOCATED_BYTES.load(Relaxed);
    SITE_EVERY.store(env_or("BOT_SITE_EVERY", 0), Relaxed);
    TRACK.store(true, Relaxed);
    tokio::time::sleep(config.track).await;
    TRACK.store(false, Relaxed);
    let tracked_end = sample(&counters);
    let tracked_rpcs = (tracked_end.0 - tracked.0) as f64;
    // SAFETY: only reads the allocator's own statistics.
    let heap = unsafe { libc::mallinfo2() };
    let live_heap = heap.uordblks + heap.hblkhd;
    let rss_peak = proc_status_kib("VmHWM");

    stop.store(true, Relaxed);
    let rpcs = (after.0 - before.0) as f64;
    let user = (spent.0 - cpu.0).as_secs_f64() * 1e6 / rpcs;
    let sys = (spent.1 - cpu.1).as_secs_f64() * 1e6 / rpcs;
    println!(
        "BOT rate={:.0} updates_per_s={:.0} user_us={user:.2} sys_us={sys:.2} cpu_us={:.2} core_share={:.3} \
         voluntary_switches_per_rpc={:.3} involuntary_switches_per_rpc={:.3} \
         allocs_per_rpc={:.2} bytes_per_rpc={:.0} p50_us={p50:.1} p95_us={p95:.1} p99_us={p99:.1} \
         rss_steady_kib={rss_steady} rss_peak_kib={rss_peak} live_heap_b={live_heap} errors={} dropped={} completed={} \
         updates_lost={} update_errors={}",
        rpcs / elapsed.as_secs_f64(),
        (after.1 - before.1) as f64 / elapsed.as_secs_f64(),
        user + sys,
        ((spent.0 - cpu.0) + (spent.1 - cpu.1)).as_secs_f64() / elapsed.as_secs_f64(),
        (switched.0 - switches.0) as f64 / rpcs,
        (switched.1 - switches.1) as f64 / rpcs,
        (ALLOCATIONS.load(Relaxed) - allocations) as f64 / tracked_rpcs,
        (ALLOCATED_BYTES.load(Relaxed) - allocated) as f64 / tracked_rpcs,
        after.2 - before.2,
        after.3 - before.3,
        after.0 - before.0,
        handle.thin.status().dropped_updates,
        counters.update_errors.load(Relaxed),
    );

    report_sites(tracked_rpcs);
    // Peers from the update stream must have reached storage, whichever kind it is.
    let mut stored = 0;
    for offset in 0..env_or("BOT_PEERS", 1i64).min(64) {
        let id = PeerId::user(OTHER_ID + offset).unwrap();
        stored += i64::from(session.peer(id).await.unwrap().is_some());
    }
    assert!(stored > 0, "peers from updates did not reach storage");

    handle.thin.quit();
    let _ = tokio::time::timeout(Duration::from_secs(2), load).await;
    consumer.abort();
}

#[test]
#[ignore = "local bot workload measurement; run explicitly with --nocapture"]
fn bot_workload() {
    let config = Config::load();
    // `BOT_MOCK_PORT` uses a mock started separately, so that a system-call tracer
    // following this process counts only the client's calls.
    if let Ok(port) = std::env::var("BOT_MOCK_PORT") {
        pin_thread("BOT_CLIENT_CPUS");
        return runtime(&config).block_on(run_client(config, port.parse().unwrap()));
    }
    let mut mock = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "bot_mock_server",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BOT_MOCK_ROLE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = StdBufReader::new(mock.stdout.take().unwrap()).lines();
    let port = loop {
        let line = lines.next().unwrap().unwrap();
        // The test harness may have printed the test's name on the same line.
        if let Some((_, port)) = line.rsplit_once("PORT ") {
            break port.trim().parse().unwrap();
        }
    };
    pin_thread("BOT_CLIENT_CPUS");
    runtime(&config).block_on(run_client(config, port));
    drop(mock.stdin.take());
    let _ = mock.wait();
}

fn runtime(config: &Config) -> tokio::runtime::Runtime {
    let mut builder = if config.multi {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(config.workers)
            .on_thread_start(|| pin_thread("BOT_CLIENT_CPUS"));
        builder
    } else {
        tokio::runtime::Builder::new_current_thread()
    };
    builder.enable_all().build().unwrap()
}
