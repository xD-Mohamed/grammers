// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Loopback measurements of a pool's per-session heap and per-RPC client cost.
//!
//! A minimal MTProto 2.0 responder on 127.0.0.1 answers with fixed replies, so
//! the measured client path is the real one: warm route, connection task,
//! abridged framing, AES-IGE/SHA-256, envelope decoding and optional gzip.
//! No Telegram endpoint, account or credential is used, and the responder
//! implements only what these measurements need. Every case is ignored:
//!
//! ```sh
//! cargo test --locked --release -p grammers-mtsender --test loopback -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Figures are local component measurements. They include this host's
//! loopback socket cost and exclude server work, which runs on other threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use grammers_crypto::aes::{ige_decrypt, ige_encrypt};
use grammers_mtsender::{
    ConnectionParams, InvocationTracker, RequestBody, SenderPool, SenderPoolFatHandle,
    SenderPoolHandle,
};
use grammers_session::SessionData;
use grammers_session::storages::MemorySession;
use grammers_tl_types::{self as tl, Identifiable, Serializable};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Allocation accounting. Responder threads are excluded so only the client's
// heap is measured.

struct Counting;

static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
const SIZE_CLASSES: usize = 16_385;
static LIVE_BY_SIZE: [AtomicI64; SIZE_CLASSES] = [const { AtomicI64::new(0) }; SIZE_CLASSES];

thread_local! {
    static RESPONDER: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    TRACK.load(Relaxed) && !RESPONDER.with(Cell::get)
}

fn note_alloc(size: usize) {
    if counted() {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Relaxed);
        LIVE_BYTES.fetch_add(size as i64, Relaxed);
        LIVE_BY_SIZE[size.min(SIZE_CLASSES - 1)].fetch_add(1, Relaxed);
    }
}

fn note_free(size: usize) {
    if counted() {
        LIVE_BYTES.fetch_sub(size as i64, Relaxed);
        LIVE_BY_SIZE[size.min(SIZE_CLASSES - 1)].fetch_sub(1, Relaxed);
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
        note_free(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            note_free(layout.size());
            note_alloc(new_size);
        }
        new
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Live client heap, in total and by exact allocation size.
struct HeapSnapshot {
    live: i64,
    by_size: Vec<i64>,
}

impl HeapSnapshot {
    /// Storage is allocated before the reading so it does not count itself.
    fn empty() -> Self {
        Self {
            live: 0,
            by_size: vec![0; SIZE_CLASSES],
        }
    }

    fn take(&mut self) {
        self.live = LIVE_BYTES.load(Relaxed);
        for (slot, count) in self.by_size.iter_mut().zip(&LIVE_BY_SIZE) {
            *slot = count.load(Relaxed);
        }
    }

    /// The largest per-session contributors between two snapshots.
    fn report(&self, base: &Self, sessions: usize, label: &str) {
        let per_session = (self.live - base.live) as f64 / sessions as f64;
        println!("{label}: {per_session:.0} B/session live client heap");
        let mut rows: Vec<(usize, i64)> = (0..SIZE_CLASSES)
            .map(|size| (size, self.by_size[size] - base.by_size[size]))
            .filter(|(_, count)| *count > 0)
            .collect();
        rows.sort_by_key(|(size, count)| std::cmp::Reverse(*size as i64 * count));
        for (size, count) in rows.into_iter().take(14) {
            let suffix = if size == SIZE_CLASSES - 1 { "+" } else { "" };
            println!(
                "    {:>6}{suffix} B x {:>6.2}/session = {:>7.0} B",
                size,
                count as f64 / sessions as f64,
                (size as i64 * count) as f64 / sessions as f64
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Client thread CPU time.

#[derive(Clone, Copy, Default)]
struct ThreadCpu {
    user: Duration,
    kernel: Duration,
    cycles: u64,
}

#[cfg(windows)]
fn thread_cpu() -> ThreadCpu {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn GetThreadTimes(
            thread: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    let mut cycles = 0;
    let span = |time: FileTime| {
        Duration::from_nanos(((u64::from(time.high) << 32) | u64::from(time.low)) * 100)
    };
    // SAFETY: pseudo-handle of the calling thread and valid out-pointers.
    unsafe {
        let thread = GetCurrentThread();
        assert_ne!(
            GetThreadTimes(thread, &mut creation, &mut exit, &mut kernel, &mut user),
            0
        );
        assert_ne!(QueryThreadCycleTime(thread, &mut cycles), 0);
    }
    ThreadCpu {
        user: span(user),
        kernel: span(kernel),
        cycles,
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu() -> ThreadCpu {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the valid out-pointer on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let duration =
        |v: libc::timeval| Duration::from_micros(v.tv_sec as u64 * 1_000_000 + v.tv_usec as u64);
    ThreadCpu {
        user: duration(usage.ru_utime),
        kernel: duration(usage.ru_stime),
        cycles: 0,
    }
}

fn pin_thread(variable: &str) {
    #[cfg(target_os = "linux")]
    if let Ok(cpus) = std::env::var(variable) {
        let mut set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
        for cpu in cpus.split(',').map(|cpu| cpu.parse::<usize>().unwrap()) {
            assert!(cpu < libc::CPU_SETSIZE as usize);
            unsafe {
                libc::CPU_SET(cpu, &mut set);
            }
        }
        // SAFETY: valid initialized affinity set; pid 0 selects the calling thread.
        assert_eq!(
            unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) },
            0
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = variable;
}

fn bench_window() -> Duration {
    Duration::from_millis(std::env::var("LOOPBACK_WINDOW_MS").map_or(4000, |n| n.parse().unwrap()))
}

// ---------------------------------------------------------------------------
// Minimal responder.

const API_ID: i32 = 1;
const HOME_DC: i32 = 2;
const MSG_CONTAINER: u32 = 0x73f1_f8dc;
const RPC_RESULT: u32 = 0xf35c_6d01;
const GZIP_PACKED: u32 = 0x3072_cfa1;
const INVOKE_WITHOUT_UPDATES: u32 =
    tl::functions::InvokeWithoutUpdates::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID;
const INVOKE_WITH_LAYER: u32 =
    tl::functions::InvokeWithLayer::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID;

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

static MSG_ID_STEP_MS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    std::env::var("LOOPBACK_MSG_ID_STEP_MS").map_or(0, |value| value.parse().unwrap())
});

#[derive(Clone, Copy, PartialEq, Debug)]
enum Payload {
    Plain,
    Gzip,
    LargeFile,
}

/// Serialize TL `bytes`.
fn tl_bytes(data: &[u8], out: &mut Vec<u8>) {
    data.serialize(out);
}

fn user(id: i64, photo: bool) -> tl::enums::User {
    let thumb: Vec<u8> = (0..120u32)
        .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(id as u32) >> 13) as u8)
        .collect();
    tl::types::User {
        is_self: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: false,
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
        access_hash: Some(id.wrapping_mul(0x1e37_79b9_7f4a_7c15)),
        first_name: Some(format!("First{id}")),
        last_name: None,
        username: Some(format!("name{id}")),
        phone: None,
        photo: photo.then(|| {
            tl::types::UserProfilePhoto {
                has_video: false,
                personal: false,
                photo_id: id.wrapping_mul(31),
                stripped_thumb: Some(thumb),
                dc_id: 4,
            }
            .into()
        }),
        status: Some(
            tl::types::UserStatusOffline {
                was_online: 1_700_000_000 + id as i32,
            }
            .into(),
        ),
        bot_info_version: None,
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

/// A medium reply: a vector of 12 objects, half with a small thumbnail.
fn reply(payload: Payload) -> Vec<u8> {
    if payload == Payload::LargeFile {
        let mut reply = tl::types::upload::File::CONSTRUCTOR_ID.to_bytes();
        tl::types::storage::FileUnknown::CONSTRUCTOR_ID.serialize(&mut reply);
        0i32.serialize(&mut reply);
        tl_bytes(&vec![0; 1024 * 1024], &mut reply);
        return reply;
    }
    let users: Vec<tl::enums::User> = (0..12).map(|i| user(1_000 + i, i % 2 == 0)).collect();
    let plain = users.to_bytes();
    match payload {
        Payload::Plain => plain,
        Payload::Gzip => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&plain).unwrap();
            let packed = encoder.finish().unwrap();
            let mut out = GZIP_PACKED.to_le_bytes().to_vec();
            tl_bytes(&packed, &mut out);
            out
        }
        Payload::LargeFile => unreachable!(),
    }
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

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// One message envelope at `at`: its ID, body and the following offset.
fn envelope(bytes: &[u8], at: usize) -> (i64, &[u8], usize) {
    let msg_id = i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let len = read_u32(bytes, at + 12) as usize;
    (msg_id, &bytes[at + 16..at + 16 + len], at + 16 + len)
}

/// Messages in a decrypted client payload, flattening a container.
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

struct Responder {
    key: ServerKey,
    users: Vec<u8>,
    config: Vec<u8>,
    sequence: AtomicU64,
}

impl Responder {
    fn reply_to(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut constructor = read_u32(body, 0);
        if constructor == tl::types::MsgsAck::CONSTRUCTOR_ID {
            return None;
        }
        if constructor == tl::functions::PingDelayDisconnect::CONSTRUCTOR_ID {
            let ping_id = i64::from_le_bytes(body[4..12].try_into().unwrap());
            return Some(tl::enums::Pong::Pong(tl::types::Pong { msg_id: 0, ping_id }).to_bytes());
        }
        let mut at = 0;
        if constructor == INVOKE_WITHOUT_UPDATES {
            constructor = read_u32(body, 4);
            at = 4;
        }
        // A deliberately small reply, whatever the responder's payload is.
        if constructor == tl::functions::Ping::CONSTRUCTOR_ID {
            let ping_id = i64::from_le_bytes(body[at + 4..at + 12].try_into().unwrap());
            return Some(tl::enums::Pong::Pong(tl::types::Pong { msg_id: 0, ping_id }).to_bytes());
        }
        Some(if constructor == INVOKE_WITH_LAYER {
            self.config.clone()
        } else {
            self.users.clone()
        })
    }

    fn next_msg_id(&self) -> i64 {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let n = self.sequence.fetch_add(1, Relaxed) & 0x3fff_ffff;
        // Optional simulated server time per reply: a census cannot wait real hours.
        // Whole seconds stay above bit 32; the sequence keeps IDs unique and odd.
        let simulated_secs = n * *MSG_ID_STEP_MS / 1000;
        (((secs + simulated_secs) << 32) | (n << 2) | 1) as i64
    }

    async fn serve(self: Arc<Self>, stream: TcpStream) {
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
            let plain = self.key.decrypt(&mut packet);
            let session_id: [u8; 8] = plain[8..16].try_into().unwrap();
            out.clear();
            for (msg_id, body) in messages(plain) {
                let Some(result) = self.reply_to(body) else {
                    continue;
                };
                let mut message = Vec::with_capacity(64 + result.len());
                message.extend_from_slice(&[0; 8]); // salt
                message.extend_from_slice(&session_id);
                message.extend_from_slice(&self.next_msg_id().to_le_bytes());
                message.extend_from_slice(&1i32.to_le_bytes()); // content-related
                if read_u32(&result, 0) == tl::types::Pong::CONSTRUCTOR_ID {
                    let mut pong = result;
                    pong[4..12].copy_from_slice(&msg_id.to_le_bytes());
                    message.extend_from_slice(&(pong.len() as i32).to_le_bytes());
                    message.extend_from_slice(&pong);
                } else {
                    message.extend_from_slice(&((12 + result.len()) as i32).to_le_bytes());
                    message.extend_from_slice(&RPC_RESULT.to_le_bytes());
                    message.extend_from_slice(&msg_id.to_le_bytes());
                    message.extend_from_slice(&result);
                }
                self.key.encrypt_frame(message, &mut out);
            }
            if !out.is_empty() && write.write_all(&out).await.is_err() {
                return;
            }
        }
    }
}

/// Start a responder on its own runtime threads and return its address.
fn start_responder(payload: Payload, threads: usize) -> SocketAddrV4 {
    let responder = Arc::new(Responder {
        key: ServerKey::new(auth_key()),
        users: reply(payload),
        config: config_reply(),
        sequence: AtomicU64::new(0),
    });
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let SocketAddr::V4(address) = listener.local_addr().unwrap() else {
        unreachable!()
    };
    std::thread::spawn(move || {
        RESPONDER.with(|flag| flag.set(true));
        pin_thread("LOOPBACK_SERVER_CPUS");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .on_thread_start(|| {
                RESPONDER.with(|flag| flag.set(true));
                pin_thread("LOOPBACK_SERVER_CPUS");
            })
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(Arc::clone(&responder).serve(stream));
            }
        });
    });
    address
}

// ---------------------------------------------------------------------------
// Client: update delivery disabled, tracked raw invocations on established
// routes, one call in flight per pool.

struct Client {
    raw: SenderPoolHandle,
    _fat: SenderPoolFatHandle,
    tracker: InvocationTracker,
}

fn open_client(address: SocketAddrV4) -> Client {
    let mut data = SessionData {
        home_dc: HOME_DC,
        ..SessionData::default()
    };
    let home = data.dc_options.get_mut(&HOME_DC).unwrap();
    home.ipv4 = address;
    home.ipv6 = SocketAddrV6::new(address.ip().to_ipv6_mapped(), address.port(), 0, 0);
    home.auth_key = Some(auth_key());
    let pool = SenderPool::with_configuration(
        Arc::new(MemorySession::from(data)),
        API_ID,
        ConnectionParams {
            receive_updates: false,
            ..Default::default()
        },
    );
    let raw = pool.handle.thin.clone();
    tokio::spawn(pool.runner.run());
    Client {
        raw,
        _fat: pool.handle,
        tracker: InvocationTracker::default(),
    }
}

/// A small request (about 270 bytes).
fn request_body() -> RequestBody {
    tl::functions::messages::GetMessages {
        id: (0..32)
            .map(|id| tl::types::InputMessageId { id }.into())
            .collect(),
    }
    .to_bytes()
    .into()
}

async fn call(client: &Client, body: &RequestBody) -> usize {
    let permit = client
        .tracker
        .try_acquire()
        .expect("call already in flight");
    client
        .raw
        .start_raw_invoke_shared_in_dc_tracked(HOME_DC, body.clone(), permit)
        .unwrap()
        .await
        .unwrap()
        .len()
}

// Establish independent pools concurrently, outside every measurement window.
// Keeping setup serial otherwise adds seconds of idle TCP startup to each pair.
async fn warm_clients(clients: &Arc<Vec<Client>>, body: &RequestBody, second_dc: bool) {
    let mut warming = tokio::task::JoinSet::new();
    for index in 0..clients.len() {
        let clients = Arc::clone(clients);
        let body = body.clone();
        warming.spawn(async move {
            call(&clients[index], &body).await;
            if second_dc {
                clients[index]
                    .raw
                    .raw_invoke_shared_in_dc(4, body)
                    .await
                    .unwrap();
            }
        });
    }
    while let Some(result) = warming.join_next().await {
        result.unwrap();
    }
}

fn client_runtime() -> tokio::runtime::Runtime {
    pin_thread("LOOPBACK_CLIENT_CPUS");
    let multi = std::env::var("LOOPBACK_RUNTIME").is_ok_and(|s| s == "multi");
    let mut builder = if multi {
        tokio::runtime::Builder::new_multi_thread()
    } else {
        tokio::runtime::Builder::new_current_thread()
    };
    if multi {
        builder
            .worker_threads(2)
            .on_thread_start(|| pin_thread("LOOPBACK_CLIENT_CPUS"))
            .on_thread_stop(|| {
                let cpu = thread_cpu();
                println!(
                    "CLIENT_WORKER_CPU user_us={} sys_us={}",
                    cpu.user.as_micros(),
                    cpu.kernel.as_micros()
                );
            });
    }
    builder.enable_all().build().unwrap()
}

#[test]
fn maximum_download_reply_survives_real_encryption_and_transport() {
    let address = start_responder(Payload::LargeFile, 1);
    client_runtime().block_on(async {
        let client = open_client(address);
        let size = tokio::time::timeout(Duration::from_secs(5), call(&client, &request_body()))
            .await
            .unwrap();
        assert_eq!(size, 1_048_592);
        client.raw.quit();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !client.raw.status().stopped {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
fn silent_dc_does_not_block_a_healthy_cold_dc() {
    let address = start_responder(Payload::Plain, 1);
    client_runtime().block_on(async {
        let client = open_client(address);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(ipv4) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        client
            ._fat
            .session
            .set_dc_option(&grammers_session::types::DcOption {
                id: 4,
                ipv4,
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, ipv4.port(), 0, 0),
                auth_key: Some(auth_key()),
            })
            .await
            .unwrap();
        let tracker = InvocationTracker::default();
        let blocked = client
            .raw
            .start_raw_invoke_shared_in_dc_tracked(
                4,
                request_body(),
                tracker.try_acquire().unwrap(),
            )
            .unwrap();
        let _peer = listener.accept().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), call(&client, &request_body()))
                .await
                .unwrap()
                > 0
        );
        drop(blocked);
        client.raw.quit();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !client.raw.status().stopped {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
#[ignore = "local loopback retention after a large reply; waits over a minute"]
fn loopback_large_reply_retention() {
    const SESSIONS: usize = 32;
    let address = start_responder(Payload::LargeFile, 2);
    let large = request_body();
    let small: RequestBody = tl::functions::Ping { ping_id: 1 }.to_bytes().into();
    let (mut base, mut after_large, mut after_idle) = (
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
    );
    let mut clients = Vec::with_capacity(SESSIONS);
    TRACK.store(true, Relaxed);
    client_runtime().block_on(async {
        let warm = open_client(address);
        call(&warm, &large).await;
        call(&warm, &small).await;
        base.take();
        for _ in 0..SESSIONS {
            clients.push(open_client(address));
        }
        for client in &clients {
            call(client, &large).await;
            call(client, &small).await;
        }
        after_large.take();
        tokio::time::sleep(Duration::from_secs(61)).await;
        for client in &clients {
            call(client, &small).await;
        }
        after_idle.take();
    });
    TRACK.store(false, Relaxed);
    println!("{SESSIONS} sessions, one 1 MiB reply each, then small replies");
    after_large.report(&base, SESSIONS, "right after the large reply");
    after_idle.report(&base, SESSIONS, "61 seconds later, after one small reply");
}

#[test]
#[ignore = "local loopback heap census; run explicitly with --ignored --nocapture"]
fn loopback_session_footprint() {
    const SESSIONS: usize = 400;
    let address = start_responder(Payload::Gzip, 2);
    let body = request_body();
    let (mut base, mut created, mut connected, mut steady, mut saturated) = (
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
        HeapSnapshot::empty(),
    );
    let mut clients = Vec::with_capacity(SESSIONS);
    TRACK.store(true, Relaxed);
    client_runtime().block_on(async {
        // Initialize lazy runtime/library state outside the measured delta.
        let warm = open_client(address);
        for _ in 0..4 {
            call(&warm, &body).await;
        }
        base.take();
        for _ in 0..SESSIONS {
            clients.push(open_client(address));
        }
        tokio::task::yield_now().await;
        created.take();
        for client in &clients {
            call(client, &body).await;
        }
        connected.take();
        for _ in 0..8 {
            for client in &clients {
                call(client, &body).await;
            }
        }
        steady.take();
        for _ in 9..1100 {
            for client in &clients {
                call(client, &body).await;
            }
        }
        saturated.take();
    });
    TRACK.store(false, Relaxed);
    println!("{SESSIONS} sessions, one connection each, gzip-packed replies");
    created.report(&base, SESSIONS, "pool created, not connected");
    connected.report(&base, SESSIONS, "connected after one RPC");
    steady.report(&base, SESSIONS, "after nine RPCs per session");
    saturated.report(
        &base,
        SESSIONS,
        "after 1100 RPCs per session (replay cache saturated)",
    );
}

#[test]
#[ignore = "local loopback client CPU and allocation cost; run explicitly"]
fn loopback_rpc_cost() {
    const SESSIONS: usize = 64;
    const CALLERS: usize = 16;
    let window = bench_window();
    // LOOPBACK_PAYLOAD=plain|gzip selects one reply shape for A/B runs.
    let selected = std::env::var("LOOPBACK_PAYLOAD").unwrap_or_default();
    for payload in [Payload::Plain, Payload::Gzip]
        .into_iter()
        .filter(|payload| {
            selected.is_empty() || format!("{payload:?}").eq_ignore_ascii_case(&selected)
        })
    {
        let address = start_responder(payload, 4);
        let body = request_body();
        let reply_len = reply(payload).len();
        client_runtime().block_on(async {
            let clients: Arc<Vec<Client>> =
                Arc::new((0..SESSIONS).map(|_| open_client(address)).collect());
            warm_clients(&clients, &body, false).await;
            let rounds = if std::env::var_os("LOOPBACK_SHORT").is_some() { 1 } else { 5 };
            for (round, track) in [(0, false), (1, true), (2, false), (3, true), (4, false)].into_iter().take(rounds) {
                let completed = Arc::new(AtomicU64::new(0));
                let started = Instant::now();
                let deadline = started + window;
                let allocations = ALLOCATIONS.load(Relaxed);
                let allocated = ALLOCATED_BYTES.load(Relaxed);
                TRACK.store(track, Relaxed);
                let cpu = thread_cpu();
                let callers: Vec<_> = (0..CALLERS)
                    .map(|caller| {
                        let clients = Arc::clone(&clients);
                        let completed = Arc::clone(&completed);
                        let body = body.clone();
                        tokio::spawn(async move {
                            // Each caller cycles through its own pools.
                            let mine: Vec<_> = (caller..SESSIONS).step_by(CALLERS).collect();
                            let mut turn = 0;
                            // Sample 1/64 calls; reserve outside the measured call path.
                            let mut latencies = Vec::with_capacity(65536);
                            while Instant::now() < deadline {
                                let started = (turn % 64 == 0).then(Instant::now);
                                call(&clients[mine[turn % mine.len()]], &body).await;
                                if let Some(started) = started {
                                    latencies.push(started.elapsed().as_nanos() as u64);
                                }
                                turn += 1;
                                completed.fetch_add(1, Relaxed);
                            }
                            latencies
                        })
                    })
                    .collect();
                let mut latencies = Vec::new();
                for caller in callers {
                    latencies.extend(caller.await.unwrap());
                }
                let spent = thread_cpu();
                let elapsed = started.elapsed();
                TRACK.store(false, Relaxed);
                let rpcs = completed.load(Relaxed) as f64;
                latencies.sort_unstable();
                println!(
                    "{payload:?} round {round}: sampled RPC latency p50/p95/p99 {:.1}/{:.1}/{:.1} us ({} samples)",
                    latencies[latencies.len() / 2] as f64 / 1000.0,
                    latencies[latencies.len() * 95 / 100] as f64 / 1000.0,
                    latencies[latencies.len() * 99 / 100] as f64 / 1000.0,
                    latencies.len(),
                );
                if track {
                    println!(
                        "{payload:?} reply ({reply_len} B) round {round}: {:.2} allocations, {:.0} bytes allocated per RPC",
                        (ALLOCATIONS.load(Relaxed) - allocations) as f64 / rpcs,
                        (ALLOCATED_BYTES.load(Relaxed) - allocated) as f64 / rpcs,
                    );
                } else {
                    println!(
                        "{payload:?} reply ({reply_len} B) round {round}: {:.0} RPC/s; client thread {:.2} us user + {:.2} us kernel, {:.0} cycles per RPC",
                        rpcs / elapsed.as_secs_f64(),
                        (spent.user - cpu.user).as_secs_f64() * 1e6 / rpcs,
                        (spent.kernel - cpu.kernel).as_secs_f64() * 1e6 / rpcs,
                        (spent.cycles - cpu.cycles) as f64 / rpcs,
                    );
                }
            }
        });
    }
}

/// Blocking socket control uses the same framing, cryptography and replies,
/// but intentionally excludes the SDK pool, admission and async task routing.
/// Its delta therefore cannot be attributed solely to Tokio.
#[test]
#[ignore = "local non-Tokio client control"]
fn loopback_blocking_control() {
    use grammers_crypto::DequeBuffer;
    use grammers_mtproto::{
        mtp::{self, Mtp},
        transport::{self, Transport},
    };
    use std::io::Read;
    struct BlockingClient {
        socket: std::net::TcpStream,
        mtp: mtp::Encrypted,
        transport: transport::Abridged,
        outgoing: DequeBuffer<u8>,
        incoming: Vec<u8>,
    }
    impl BlockingClient {
        fn call(&mut self, body: &[u8]) {
            let id = self.mtp.push(&mut self.outgoing, body).unwrap();
            self.mtp.finalize(&mut self.outgoing).unwrap();
            self.transport.pack(&mut self.outgoing);
            self.socket.write_all(&self.outgoing[..]).unwrap();
            self.outgoing.clear();
            loop {
                if let Ok(packet) = self.transport.unpack(&mut self.incoming) {
                    let results = self
                        .mtp
                        .deserialize(&mut self.incoming[packet.data_range])
                        .unwrap();
                    self.incoming.drain(..packet.next_offset);
                    if results
                        .iter()
                        .any(|r| matches!(r, mtp::Deserialization::RpcResult(r) if r.msg_id == id))
                    {
                        return;
                    }
                }
                let mut bytes = [0; 8192];
                let count = self.socket.read(&mut bytes).unwrap();
                assert!(count > 0);
                self.incoming.extend_from_slice(&bytes[..count]);
            }
        }
    }
    let address = start_responder(Payload::Plain, 4);
    let barrier = Arc::new(std::sync::Barrier::new(17));
    let window = bench_window();
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                pin_thread("LOOPBACK_CLIENT_CPUS");
                let body = request_body();
                let mut clients: Vec<_> = (0..4)
                    .map(|_| {
                        let socket = std::net::TcpStream::connect(address).unwrap();
                        socket.set_nodelay(true).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        BlockingClient {
                            socket,
                            mtp: mtp::Encrypted::build()
                                .receive_updates(false)
                                .finish(auth_key()),
                            transport: transport::Abridged::new(),
                            outgoing: DequeBuffer::with_capacity(1024, 64),
                            incoming: Vec::with_capacity(8192),
                        }
                    })
                    .collect();
                for client in &mut clients {
                    client.call(&body);
                }
                barrier.wait();
                let cpu = thread_cpu();
                let deadline = Instant::now() + window;
                let mut count = 0;
                let mut latencies = Vec::new();
                while Instant::now() < deadline {
                    let start = Instant::now();
                    clients[count % 4].call(&body);
                    if count % 64 == 0 {
                        latencies.push(start.elapsed().as_nanos() as u64);
                    }
                    count += 1;
                }
                let spent = thread_cpu();
                println!(
                    "BLOCKING_WORKER_CPU user_us={} sys_us={} rpcs={count}",
                    (spent.user - cpu.user).as_micros(),
                    (spent.kernel - cpu.kernel).as_micros()
                );
                (
                    count,
                    spent.user - cpu.user,
                    spent.kernel - cpu.kernel,
                    latencies,
                )
            })
        })
        .collect();
    barrier.wait();
    let start = Instant::now();
    let (mut count, mut user, mut sys, mut latencies) =
        (0usize, Duration::ZERO, Duration::ZERO, Vec::new());
    for worker in workers {
        let (n, u, s, l) = worker.join().unwrap();
        count += n;
        user += u;
        sys += s;
        latencies.extend(l);
    }
    latencies.sort_unstable();
    println!(
        "BLOCKING rate={:.0} user_us={:.3} sys_us={:.3} p50_us={:.1} p95_us={:.1} p99_us={:.1}",
        count as f64 / start.elapsed().as_secs_f64(),
        user.as_secs_f64() * 1e6 / count as f64,
        sys.as_secs_f64() * 1e6 / count as f64,
        latencies[latencies.len() / 2] as f64 / 1000.0,
        latencies[latencies.len() * 95 / 100] as f64 / 1000.0,
        latencies[latencies.len() * 99 / 100] as f64 / 1000.0
    );
}

#[test]
#[ignore = "local open-loop multiple-DC and cross-pool latency/fairness"]
fn loopback_open_loop() {
    // Arrival timestamps are scheduled independently of completion. Late work
    // retains the intended arrival time; bounded-admission losses are counted.
    let large = std::env::var_os("LOOPBACK_LARGE").is_some();
    let addresses = [
        start_responder(
            if large {
                Payload::LargeFile
            } else {
                Payload::Plain
            },
            2,
        ),
        start_responder(Payload::Gzip, 2),
    ];
    let rate: usize = std::env::var("LOOPBACK_RATE")
        .map_or(if large { 100 } else { 20000 }, |s| s.parse().unwrap());
    let cold = std::env::var_os("LOOPBACK_COLD").is_some();
    let window = bench_window();
    client_runtime().block_on(async {
        let clients: Arc<Vec<_>> = Arc::new((0..64).map(|_| open_client(addresses[0])).collect());
        for client in clients.iter() {
            client._fat.session.set_dc_option(&grammers_session::types::DcOption {
                id: 4, ipv4: addresses[1], ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, addresses[1].port(), 0, 0), auth_key: Some(auth_key()),
            }).await.unwrap();
            if !cold {
                call(client, &request_body()).await;
                client.raw.raw_invoke_in_dc(4, request_body().to_vec()).await.unwrap();
            }
        }
        let body = request_body();
        let start = tokio::time::Instant::now();
        let total = (window.as_secs_f64() * rate as f64) as usize;
        let mut tasks = tokio::task::JoinSet::new();
        let mut rows = Vec::with_capacity(total);
        let mut dropped = 0;
        let mut peak = 0;
        for index in 0..total {
            while let Some(row) = tasks.try_join_next() { rows.push(row.unwrap()); }
            let due = start + Duration::from_secs_f64(index as f64 / rate as f64);
            tokio::time::sleep_until(due).await;
            if tasks.len() >= 1024 { dropped += 1; continue; }
            let clients = Arc::clone(&clients);
            let body = body.clone();
            tasks.spawn(async move {
                // Alternate the quiet and busy pool groups. Busy pools receive
                // bursts through a single shared pool, quiet pools rotate.
                let group = index % 2;
                let pool = if group == 0 { 0 } else { 1 + index % 63 };
                let dc = if index % 4 < 2 { HOME_DC } else { 4 };
                let result = tokio::time::timeout(Duration::from_secs(5), clients[pool].raw.raw_invoke_shared_in_dc(dc, body)).await;
                (group, due.elapsed().as_nanos() as u64, result.is_ok_and(|r| r.is_ok()))
            });
            peak = peak.max(tasks.len());
        }
        while let Some(row) = tasks.join_next().await { rows.push(row.unwrap()); }
        let elapsed = start.elapsed();
        for group in [0, 1] {
            let mut latency: Vec<_> = rows.iter().filter(|r| r.0 == group && r.2).map(|r| r.1).collect();
            latency.sort_unstable();
            let errors = rows.iter().filter(|r| r.0 == group && !r.2).count();
            if latency.is_empty() { println!("OPEN group={group} completed=0 errors={errors} dropped={dropped} peak={peak}"); continue; }
            println!("OPEN group={group} cold={cold} large={large} scheduled={total} completed={} errors={errors} dropped={dropped} peak={peak} rate={:.0} p50_us={:.1} p95_us={:.1} p99_us={:.1}",
                latency.len(), latency.len() as f64/elapsed.as_secs_f64(), latency[latency.len()/2] as f64/1000.0,
                latency[latency.len()*95/100] as f64/1000.0, latency[latency.len()*99/100] as f64/1000.0);
        }
        for client in clients.iter() { client.raw.quit(); }
    });
}
