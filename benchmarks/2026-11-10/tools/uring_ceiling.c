/*
 * uring_ceiling: an upper bound on what batched io_uring socket I/O could save the
 * client thread per request, against the readiness (epoll) pattern the library uses
 * today. Measurement tool only; nothing in the library depends on it.
 *
 *   gcc -O2 -Wall -Wextra -pthread -o "$TMPDIR/uring_ceiling" uring_ceiling.c
 *
 * One process. A server thread answers `conns` loopback TCP connections; the main
 * thread is the client and keeps exactly `inflight` requests in flight over them, one
 * per connection at most, a `req`-byte request and a `rep`-byte reply. When a reply
 * completes the next request starts on the next idle connection in round-robin order.
 * The server is the same code for every mode.
 *
 * --mode epoll models the library on tokio: nonblocking sockets registered once,
 *   edge-triggered. A request is one send(). A readable event allows one recv() into a
 *   4096-byte buffer; readiness is forgotten when a recv returns fewer bytes than it
 *   asked for, or EAGAIN, as tokio does. Every event of one epoll_wait is handled
 *   before the next epoll_wait.
 *
 * --mode uring models the best case for batching on one ring: a request queues one
 *   IORING_OP_SEND and one IORING_OP_RECV without submitting. The loop reaps every
 *   available completion, starting the next request for each completed reply, and only
 *   then makes one io_uring_enter that submits everything queued and waits.
 *   --uring-send-inline sends with a plain send() and puts only the recv on the ring.
 *   --uring-wait-past-sends is not in the original request. A SEND completes during the
 *   very io_uring_enter that submits it, so a wait for one completion returns at once
 *   holding only SEND completions and a second call is needed to sleep: two calls for a
 *   lone request. Linux 5.15 cannot suppress those completions (IOSQE_CQE_SKIP_SUCCESS
 *   is 5.17), so this flag waits for one completion more than the SENDs the same call
 *   submits. That relies on a small request leaving in one SEND; see client_run_uring.
 *
 * What it measures: the client thread's CPU per request (getrusage(RUSAGE_THREAD)) and
 * its system calls by kind, inside a window that starts after the warm-up. Only the
 * kernel interface differs between the modes. `--work-iters` adds the same user-space
 * work per reply to every mode, standing in for the decryption and bookkeeping the
 * real client does between two I/O calls: it decides how often the client is found
 * running when a reply arrives, which is what the two interfaces treat differently.
 *
 * What it cannot tell:
 * - Nothing about the library itself. No tokio, wakers, MTProto, AES or allocation runs
 *   here, and a reactor built on a ring would add costs this loop does not have
 *   (cancellation, timers, a buffer committed to every waiting recv).
 * - It is a ceiling for 5.15 only. A recv that has to wait is attempted twice, and its
 *   completion is run as task work signalled with TWA_SIGNAL: an interrupt from the
 *   server's CPU if the client is running, where epoll only queues an event. The
 *   output counts them (`resched_irq_per_rpc`, the whole host's). Later kernels
 *   (COOP_TASKRUN, DEFER_TASKRUN, multishot recv, buffer rings) behave differently.
 * - Loopback: a send() also runs the receiver's side of TCP, the peer answers within
 *   microseconds and never loses a packet. Over a real network replies arrive further
 *   apart and there is less to batch.
 * - A closed loop at full speed. A lightly loaded client wakes once per reply in
 *   either mode.
 * - The split between user and system time is sampled at the scheduler tick here
 *   (CONFIG_TICK_CPU_ACCOUNTING, 250 Hz) and scaled to the exact total: trust
 *   cpu_us_per_rpc first, the split only over many runs.
 * - CPU spent by io_uring worker threads would not be the client thread's. No request
 *   here should reach one; `other_cpu_us` reports the process's CPU outside the two
 *   threads so that this can be checked.
 *
 * The clock is a system call on this host, so the loop never reads it. The stop
 * condition and the latency samples use the TSC, calibrated against CLOCK_MONOTONIC
 * before the run and again over the window itself.
 *
 * Standard output is exactly one JSON object on one line. Diagnostics go to stderr.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <inttypes.h>
#include <linux/io_uring.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <semaphore.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>
#include <x86intrin.h>

#if !defined(__x86_64__) || !defined(__linux__)
#error "uring_ceiling reads the TSC and calls io_uring directly: x86-64 Linux only"
#endif

#define TAG_BYTES 8u
#define RECV_CHUNK 4096u /* what one recv asks for, at most: the library's read buffer */
#define EPOLL_BATCH 1024
#define SQ_ENTRIES 256u
#define SMALL_SEND 4096u /* --uring-wait-past-sends: a request taken to leave in one SEND */
#define LAT_CAP (1u << 21) /* latency samples kept; a uniform reservoir beyond that */
#define WAKE_TOKEN UINT32_MAX
#define MAX_CONNS 8192
#define MAX_MESSAGE (1u << 20)

enum variant { V_EPOLL, V_URING, V_URING_SEND_INLINE, V_URING_WAIT_PAST_SENDS };
static const char *const variant_names[] = { "epoll", "uring", "uring-send-inline",
                                             "uring-wait-past-sends" };

struct config {
    enum variant variant;
    int conns;
    int inflight;
    uint32_t req;
    uint32_t rep;
    uint64_t work_iters;
    double seconds;
    double warmup;
    int pin_client; /* -1: not pinned */
    int pin_server;
    bool calibrate;
};

__attribute__((noreturn, format(printf, 1, 2))) static void die(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    fputs("uring_ceiling: fatal: ", stderr);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    _exit(2);
}

static void *xcalloc(size_t count, size_t size)
{
    void *p = calloc(count, size);
    if (!p)
        die("out of memory");
    return p;
}

static void fill_pattern(uint8_t *buf, size_t len, unsigned seed)
{
    for (size_t i = 0; i < len; i++)
        buf[i] = (uint8_t)(i * 131u + seed);
}

static void pin_self(int cpu, const char *who)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    if (cpu >= CPU_SETSIZE)
        die("cpu %d is out of range", cpu);
    CPU_SET(cpu, &set);
    int rc = pthread_setaffinity_np(pthread_self(), sizeof set, &set);
    if (rc)
        die("cannot pin the %s thread to cpu %d: %s", who, cpu, strerror(rc));
}

static void set_nodelay(int fd)
{
    int one = 1;
    if (setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one) < 0)
        die("TCP_NODELAY: %s", strerror(errno));
}

/* ---- time: the TSC, never the clock, while requests are running ---- */

struct clockpair {
    uint64_t tsc;
    uint64_t ns;
};

/* One CLOCK_MONOTONIC reading and the TSC at the same instant: the tightest of three. */
static struct clockpair clock_pair(void)
{
    struct clockpair best = { 0, 0 };
    uint64_t best_gap = UINT64_MAX;
    for (int i = 0; i < 3; i++) {
        struct timespec ts;
        uint64_t before = __rdtsc();
        if (clock_gettime(CLOCK_MONOTONIC, &ts) < 0)
            die("clock_gettime: %s", strerror(errno));
        uint64_t after = __rdtsc();
        if (after - before < best_gap) {
            best_gap = after - before;
            best.tsc = before + (after - before) / 2;
            best.ns = (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
        }
    }
    return best;
}

static double tsc_hz_between(struct clockpair a, struct clockpair b)
{
    if (b.ns <= a.ns || b.tsc <= a.tsc)
        die("the TSC or the clock did not advance");
    return (double)(b.tsc - a.tsc) * 1e9 / (double)(b.ns - a.ns);
}

static double tsc_hz_initial(void)
{
    const struct timespec nap = { 0, 10 * 1000 * 1000 };
    struct clockpair a = clock_pair();
    nanosleep(&nap, NULL);
    return tsc_hz_between(a, clock_pair());
}

/* ---- the work between two I/O calls ---- */

static volatile uint64_t g_sink;

/*
 * One iteration reads one 8-byte word of the reply and feeds a multiply chain, so the
 * cost is serial and the compiler cannot vectorise it away. Not inlined, so that
 * --calibrate times the same code as the run. The caller guarantees len >= 8.
 */
__attribute__((noinline)) static uint64_t mix_work(const uint8_t *buf, uint32_t len,
                                                   uint64_t iters)
{
    const uint32_t words = len / 8;
    uint64_t h = 0x9E3779B97F4A7C15ull ^ len;
    uint32_t w = 0;
    for (uint64_t i = 0; i < iters; i++) {
        uint64_t v;
        memcpy(&v, buf + (size_t)w * 8, sizeof v);
        h = (h ^ v) * 0xFF51AFD7ED558CCDull;
        h ^= h >> 32;
        if (++w == words)
            w = 0;
    }
    return h;
}

/* ---- server: identical for every mode ---- */

struct sconn {
    int fd;
    uint32_t rgot; /* request bytes buffered */
    uint32_t wpos; /* reply bytes written */
    bool writing;  /* a reply did not fit the socket: waiting for EPOLLOUT */
    uint8_t *rbuf;
    uint8_t *wbuf;
};

struct server {
    const struct config *cfg;
    int listen_fd;
    int wake_fd;
    int ep;
    sem_t ready;
    struct sconn *conns;
};

static void server_close(struct server *s, struct sconn *c)
{
    epoll_ctl(s->ep, EPOLL_CTL_DEL, c->fd, NULL);
    close(c->fd);
    c->fd = -1;
}

static void server_watch(struct server *s, struct sconn *c, uint32_t events)
{
    struct epoll_event ev = { .events = events, .data = { .u32 = (uint32_t)(c - s->conns) } };
    if (epoll_ctl(s->ep, EPOLL_CTL_MOD, c->fd, &ev) < 0)
        die("server epoll_ctl: %s", strerror(errno));
}

static void server_flush(struct server *s, struct sconn *c)
{
    const uint32_t rep = s->cfg->rep;
    while (c->wpos < rep) {
        ssize_t n = send(c->fd, c->wbuf + c->wpos, rep - c->wpos, MSG_NOSIGNAL);
        if (n > 0) {
            c->wpos += (uint32_t)n;
            continue;
        }
        if (n < 0 && errno == EINTR)
            continue;
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            if (!c->writing) {
                c->writing = true; /* no further request is read until this is out */
                server_watch(s, c, EPOLLOUT);
            }
            return;
        }
        if (n < 0 && (errno == EPIPE || errno == ECONNRESET)) {
            server_close(s, c);
            return;
        }
        die("server send: %s", n < 0 ? strerror(errno) : "nothing written");
    }
    if (c->writing) {
        c->writing = false;
        server_watch(s, c, EPOLLIN);
    }
}

static void server_readable(struct server *s, struct sconn *c)
{
    const uint32_t req = s->cfg->req;
    ssize_t n = recv(c->fd, c->rbuf + c->rgot, req - c->rgot, 0);
    if (n > 0) {
        c->rgot += (uint32_t)n;
        if (c->rgot < req)
            return; /* level-triggered: the rest is reported when it arrives */
        memcpy(c->wbuf, c->rbuf, TAG_BYTES);
        c->rgot = 0;
        c->wpos = 0;
        server_flush(s, c);
        return;
    }
    if (n == 0 || errno == ECONNRESET) {
        server_close(s, c);
        return;
    }
    if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)
        die("server recv: %s", strerror(errno));
}

static void *server_main(void *arg)
{
    struct server *s = arg;
    const struct config *cfg = s->cfg;
    static struct epoll_event evs[EPOLL_BATCH];

    if (cfg->pin_server >= 0)
        pin_self(cfg->pin_server, "server");
    s->ep = epoll_create1(EPOLL_CLOEXEC);
    if (s->ep < 0)
        die("server epoll_create1: %s", strerror(errno));
    for (int i = 0; i < cfg->conns; i++) {
        struct sconn *c = &s->conns[i];
        do
            c->fd = accept4(s->listen_fd, NULL, NULL, SOCK_NONBLOCK | SOCK_CLOEXEC);
        while (c->fd < 0 && errno == EINTR);
        if (c->fd < 0)
            die("accept: %s", strerror(errno));
        set_nodelay(c->fd);
        fill_pattern(c->wbuf, cfg->rep, 89);
        struct epoll_event ev = { .events = EPOLLIN, .data = { .u32 = (uint32_t)i } };
        if (epoll_ctl(s->ep, EPOLL_CTL_ADD, c->fd, &ev) < 0)
            die("server epoll_ctl: %s", strerror(errno));
    }
    struct epoll_event wake = { .events = EPOLLIN, .data = { .u32 = WAKE_TOKEN } };
    if (epoll_ctl(s->ep, EPOLL_CTL_ADD, s->wake_fd, &wake) < 0)
        die("server epoll_ctl: %s", strerror(errno));
    sem_post(&s->ready);

    for (;;) {
        int n = epoll_wait(s->ep, evs, EPOLL_BATCH, -1);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            die("server epoll_wait: %s", strerror(errno));
        }
        for (int i = 0; i < n; i++) {
            uint32_t token = evs[i].data.u32;
            if (token == WAKE_TOKEN)
                return NULL; /* the client has closed the window and read our CPU */
            struct sconn *c = &s->conns[token];
            if (c->fd < 0)
                continue;
            if (c->writing)
                server_flush(s, c);
            else
                server_readable(s, c);
        }
    }
}

/* ---- client ---- */

struct cconn {
    int fd;
    uint32_t got;  /* reply bytes received */
    uint32_t sent; /* request bytes a SEND completion has confirmed (ring only) */
    bool busy;     /* a request is in flight */
    bool readable; /* epoll: readiness reported and not yet used up */
    uint64_t tag;
    uint64_t t0; /* TSC when the request was issued or queued */
    uint8_t *sbuf;
    uint8_t *rbuf;
};

/* Everything here is zeroed when the window opens. */
struct counters {
    uint64_t rpcs;
    uint64_t send, recv, epoll_wait, uring_enter, poll;
    uint64_t sqes, cqes;
    uint64_t partial_recv; /* a recv or RECV that left the reply incomplete */
    uint64_t eagain_recv;  /* a recv() that found nothing */
    uint64_t send_retry;   /* a send cut short or made to wait: outside the model */
};

/*
 * The submission and completion rings, mapped from the kernel. The kernel writes the
 * SQ head and the CQ tail, this thread writes the SQ tail and the CQ head; each shared
 * index is read with an acquire load and written with a release store, and this thread
 * keeps its own copy of the two it owns.
 */
struct ring {
    int fd;
    uint32_t features;
    uint32_t sq_entries, sq_mask, cq_entries, cq_mask;
    uint32_t *sq_ktail, *sq_kdropped;
    uint32_t *cq_khead, *cq_ktail, *cq_koverflow;
    struct io_uring_sqe *sqes;
    struct io_uring_cqe *cqes;
    uint32_t sq_tail;      /* next slot to fill */
    uint32_t to_submit;    /* queued, not yet taken by the kernel */
    uint32_t queued_sends; /* SENDs among those */
    uint32_t cq_head;      /* next completion to read */
};

enum phase { PH_WARMUP, PH_MEASURE, PH_DONE };

/* What is read at each edge of the window. */
struct edge {
    struct clockpair wall;
    uint64_t client_ns;  /* this thread's CPU clock */
    uint64_t server_ns;  /* the server thread's CPU clock, read from here */
    uint64_t process_ns; /* every thread of the process */
    uint64_t resched;    /* rescheduling interrupts, every CPU of the host */
    struct rusage ru;    /* this thread: the user and system split, and its switches */
};

struct client {
    const struct config *cfg;
    enum variant variant;
    int nconns;
    uint32_t req, rep, rbuf_cap;
    uint64_t work_iters;
    struct cconn *conns;
    int rr; /* round-robin cursor */
    uint32_t confirmed; /* ring: requests a SEND completion showed wholly sent, reply due */
    uint64_t seq;
    enum phase phase;
    uint64_t deadline; /* TSC */
    double tsc_hz;
    struct counters n;
    uint32_t *lat; /* TSC ticks per request, saturating */
    uint64_t lat_seen;
    uint64_t rng;
    int ep;
    struct ring ring;
    int wake_fd;
    clockid_t server_clock;
    struct edge edge[2];
};

static void ring_init(struct ring *r, uint32_t sq_entries, uint32_t cq_entries)
{
    struct io_uring_params p;
    memset(&p, 0, sizeof p);
    p.flags = IORING_SETUP_CQSIZE;
    p.cq_entries = cq_entries;
    r->fd = (int)syscall(__NR_io_uring_setup, sq_entries, &p);
    if (r->fd < 0)
        die("io_uring_setup: %s", strerror(errno));
    if (!(p.features & IORING_FEAT_FAST_POLL))
        die("this kernel has no IORING_FEAT_FAST_POLL: every recv would go to a worker");

    size_t sq_len = p.sq_off.array + p.sq_entries * sizeof(uint32_t);
    size_t cq_len = p.cq_off.cqes + p.cq_entries * sizeof(struct io_uring_cqe);
    const int prot = PROT_READ | PROT_WRITE, flags = MAP_SHARED | MAP_POPULATE;
    char *sq, *cq;
    if (p.features & IORING_FEAT_SINGLE_MMAP) {
        if (cq_len > sq_len)
            sq_len = cq_len;
        sq = cq = mmap(NULL, sq_len, prot, flags, r->fd, IORING_OFF_SQ_RING);
    } else {
        sq = mmap(NULL, sq_len, prot, flags, r->fd, IORING_OFF_SQ_RING);
        cq = mmap(NULL, cq_len, prot, flags, r->fd, IORING_OFF_CQ_RING);
    }
    r->sqes = mmap(NULL, p.sq_entries * sizeof(struct io_uring_sqe), prot, flags, r->fd,
                   IORING_OFF_SQES);
    if (sq == MAP_FAILED || cq == MAP_FAILED || (void *)r->sqes == MAP_FAILED)
        die("mmap of the rings: %s", strerror(errno));

    r->features = p.features;
    r->sq_entries = p.sq_entries;
    r->cq_entries = p.cq_entries;
    r->sq_mask = *(uint32_t *)(sq + p.sq_off.ring_mask);
    r->cq_mask = *(uint32_t *)(cq + p.cq_off.ring_mask);
    r->sq_ktail = (uint32_t *)(sq + p.sq_off.tail);
    r->sq_kdropped = (uint32_t *)(sq + p.sq_off.dropped);
    r->cq_khead = (uint32_t *)(cq + p.cq_off.head);
    r->cq_ktail = (uint32_t *)(cq + p.cq_off.tail);
    r->cq_koverflow = (uint32_t *)(cq + p.cq_off.overflow);
    r->cqes = (struct io_uring_cqe *)(cq + p.cq_off.cqes);
    /* The indirection array never changes: slot i of the ring is entry i. */
    uint32_t *array = (uint32_t *)(sq + p.sq_off.array);
    for (uint32_t i = 0; i < p.sq_entries; i++)
        array[i] = i;
    r->sq_tail = __atomic_load_n(r->sq_ktail, __ATOMIC_ACQUIRE);
    r->cq_head = __atomic_load_n(r->cq_khead, __ATOMIC_ACQUIRE);
    r->to_submit = 0;
    r->queued_sends = 0;
}

/*
 * The one system call of ring mode: hand over everything queued and, if min_complete
 * is not zero, wait until that many completions are in the ring.
 */
static void ring_enter(struct client *cl, uint32_t min_complete)
{
    struct ring *r = &cl->ring;
    /* The kernel reads the tail to find the new entries, and the head to count the
     * completions already waiting and the room left for more. */
    __atomic_store_n(r->sq_ktail, r->sq_tail, __ATOMIC_RELEASE);
    __atomic_store_n(r->cq_khead, r->cq_head, __ATOMIC_RELEASE);
    for (;;) {
        cl->n.uring_enter++;
        long ret = syscall(__NR_io_uring_enter, r->fd, r->to_submit, min_complete,
                           min_complete ? IORING_ENTER_GETEVENTS : 0, NULL, 0);
        if (ret < 0) {
            if (errno == EINTR)
                continue; /* only returned when nothing was submitted */
            die("io_uring_enter: %s", strerror(errno));
        }
        if ((uint32_t)ret > r->to_submit || (ret == 0 && r->to_submit != 0))
            die("io_uring_enter took %ld of %u queued entries", ret, r->to_submit);
        /* After a partial submission the kernel did not wait; the caller's loop comes
         * back here with the rest. */
        r->to_submit -= (uint32_t)ret;
        r->queued_sends = 0;
        return;
    }
}

static struct io_uring_sqe *ring_get_sqe(struct client *cl)
{
    struct ring *r = &cl->ring;
    if (r->to_submit == r->sq_entries)
        ring_enter(cl, 0); /* the submission ring is full: submit early, do not wait */
    struct io_uring_sqe *sqe = &r->sqes[r->sq_tail & r->sq_mask];
    r->sq_tail++;
    r->to_submit++;
    cl->n.sqes++;
    memset(sqe, 0, sizeof *sqe);
    return sqe;
}

static void uring_queue_send(struct client *cl, struct cconn *c)
{
    struct io_uring_sqe *sqe = ring_get_sqe(cl);
    sqe->opcode = IORING_OP_SEND;
    sqe->fd = c->fd;
    sqe->addr = (uint64_t)(uintptr_t)(c->sbuf + c->sent);
    sqe->len = cl->req - c->sent;
    sqe->msg_flags = MSG_NOSIGNAL;
    sqe->user_data = (uint64_t)(c - cl->conns) << 1;
    cl->ring.queued_sends++;
}

static void uring_queue_recv(struct client *cl, struct cconn *c)
{
    uint32_t ask = cl->rbuf_cap - c->got;
    struct io_uring_sqe *sqe = ring_get_sqe(cl);
    sqe->opcode = IORING_OP_RECV;
    sqe->fd = c->fd;
    sqe->addr = (uint64_t)(uintptr_t)(c->rbuf + c->got);
    sqe->len = ask < RECV_CHUNK ? ask : RECV_CHUNK;
    sqe->user_data = (uint64_t)(c - cl->conns) << 1 | 1;
}

/* A request that one send() did not finish. Not part of the model: finish it by
 * waiting for the socket, and count it so that such a run can be recognised. */
static void client_send_slow(struct client *cl, struct cconn *c, ssize_t n, int err)
{
    uint32_t off = 0;
    for (;;) {
        if (n > 0)
            off += (uint32_t)n;
        else if (n == 0 || (err != EAGAIN && err != EWOULDBLOCK && err != EINTR))
            die("send on connection %td: %s", c - cl->conns,
                n ? strerror(err) : "nothing written");
        if (off == cl->req)
            return;
        cl->n.send_retry++;
        if (n < 0 && err != EINTR) {
            struct pollfd p = { .fd = c->fd, .events = POLLOUT };
            cl->n.poll++;
            if (poll(&p, 1, -1) < 0 && errno != EINTR)
                die("poll: %s", strerror(errno));
        }
        cl->n.send++;
        n = send(c->fd, c->sbuf + off, cl->req - off, MSG_NOSIGNAL);
        err = errno;
    }
}

static inline void client_send_now(struct client *cl, struct cconn *c)
{
    cl->n.send++;
    ssize_t n = send(c->fd, c->sbuf, cl->req, MSG_NOSIGNAL);
    if (__builtin_expect(n != (ssize_t)cl->req, 0))
        client_send_slow(cl, c, n, errno);
}

/* Start one request on the next idle connection in round-robin order. The caller
 * guarantees that fewer than `conns` requests are in flight. */
static struct cconn *start_next(struct client *cl)
{
    const int n = cl->nconns;
    int i = cl->rr;
    for (int scanned = 0; cl->conns[i].busy; scanned++) {
        if (scanned == n)
            die("internal error: no idle connection");
        if (++i == n)
            i = 0;
    }
    cl->rr = i + 1 == n ? 0 : i + 1;

    struct cconn *c = &cl->conns[i];
    c->busy = true;
    c->got = 0;
    c->sent = 0;
    c->tag = ++cl->seq * 0x9E3779B97F4A7C15ull; /* odd multiplier: never repeats */
    memcpy(c->sbuf, &c->tag, TAG_BYTES);
    c->t0 = __rdtsc();
    switch (cl->variant) {
    case V_EPOLL:
        client_send_now(cl, c);
        break;
    case V_URING_SEND_INLINE:
        client_send_now(cl, c);
        uring_queue_recv(cl, c);
        break;
    default:
        uring_queue_send(cl, c);
        uring_queue_recv(cl, c);
        break;
    }
    return c;
}

static inline void record_latency(struct client *cl, uint64_t ticks)
{
    uint32_t v = ticks > UINT32_MAX ? UINT32_MAX : (uint32_t)ticks;
    uint64_t k = cl->lat_seen++;
    if (k < LAT_CAP) {
        cl->lat[k] = v;
        return;
    }
    cl->rng ^= cl->rng << 13;
    cl->rng ^= cl->rng >> 7;
    cl->rng ^= cl->rng << 17;
    uint64_t j = cl->rng % (k + 1);
    if (j < LAT_CAP)
        cl->lat[j] = v;
}

static uint64_t cpu_clock_ns(clockid_t id)
{
    struct timespec ts;
    if (clock_gettime(id, &ts) < 0)
        die("CPU clock: %s", strerror(errno));
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

/*
 * This thread's CPU. On its own, getrusage(RUSAGE_THREAD) reports the run time as of
 * the last scheduler tick or switch, up to 4 ms ago on this kernel. Reading the
 * thread's CPU clock makes the kernel bring that figure up to date, so the clock is
 * read first and the two then agree to a microsecond.
 */
static void client_cpu(struct edge *e)
{
    e->client_ns = cpu_clock_ns(CLOCK_THREAD_CPUTIME_ID);
    if (getrusage(RUSAGE_THREAD, &e->ru) < 0)
        die("getrusage: %s", strerror(errno));
}

/* The server's CPU at the same instant, through its thread clock, which is exact for
 * a thread that is running elsewhere. The process clock is only as fresh as each
 * thread's last update, hence the order. */
static void others_cpu(const struct client *cl, struct edge *e)
{
    e->server_ns = cpu_clock_ns(cl->server_clock);
    e->process_ns = cpu_clock_ns(CLOCK_PROCESS_CPUTIME_ID);
}

/*
 * Rescheduling interrupts taken so far, over every CPU: the RES line of
 * /proc/interrupts. This is how 5.15 tells a running thread that one of its ring's
 * operations can complete, and under a hypervisor an interrupt between CPUs is dear.
 * The whole host is counted, so other processes add to it. Zero if unreadable.
 */
static uint64_t resched_interrupts(void)
{
    FILE *f = fopen("/proc/interrupts", "r");
    char *line = NULL;
    size_t cap = 0;
    uint64_t total = 0;
    if (!f)
        return 0;
    while (getline(&line, &cap, f) > 0) {
        char *p = line + strspn(line, " ");
        if (strncmp(p, "RES:", 4))
            continue;
        for (p += 4;;) {
            char *end;
            unsigned long long v = strtoull(p, &end, 10);
            if (end == p)
                break;
            total += v;
            p = end;
        }
        break;
    }
    free(line);
    fclose(f);
    return total;
}

/*
 * The two edges of the window. Each is taken between two requests: after a reply was
 * handled and before the next request is issued, so the window holds whole requests.
 * The thread's own CPU is read last when opening and first when closing, which keeps
 * the other readings out of it.
 */
static void advance_phase(struct client *cl)
{
    if (cl->phase == PH_WARMUP) {
        struct edge *e = &cl->edge[0];
        memset(&cl->n, 0, sizeof cl->n);
        cl->lat_seen = 0;
        e->resched = resched_interrupts();
        others_cpu(cl, e);
        e->wall = clock_pair();
        client_cpu(e);
        cl->deadline = __rdtsc() + (uint64_t)(cl->cfg->seconds * cl->tsc_hz);
        cl->phase = PH_MEASURE;
    } else {
        struct edge *e = &cl->edge[1];
        client_cpu(e);
        e->wall = clock_pair();
        others_cpu(cl, e);
        e->resched = resched_interrupts();
        const uint64_t one = 1; /* the server may leave now */
        if (write(cl->wake_fd, &one, sizeof one) != sizeof one)
            die("eventfd write: %s", strerror(errno));
        cl->phase = PH_DONE;
    }
}

/* A whole reply is in c->rbuf. Returns the connection the next request started on, or
 * NULL when the run is over. */
static struct cconn *reply_done(struct client *cl, struct cconn *c)
{
    const uint64_t now = __rdtsc();
    uint64_t echoed;
    memcpy(&echoed, c->rbuf, TAG_BYTES);
    if (echoed != c->tag)
        die("connection %td: reply carries tag %016" PRIx64 ", request had %016" PRIx64,
            c - cl->conns, echoed, c->tag);
    if (cl->work_iters)
        g_sink = mix_work(c->rbuf, cl->rep, cl->work_iters);
    if (cl->phase == PH_MEASURE) {
        cl->n.rpcs++;
        record_latency(cl, now - c->t0);
    }
    c->busy = false;
    if (now >= cl->deadline) {
        advance_phase(cl);
        if (cl->phase == PH_DONE)
            return NULL;
    }
    return start_next(cl);
}

/* epoll mode: use the readiness of a connection with a request in flight. Returns
 * true when its reply is complete. */
static bool epoll_recv(struct client *cl, struct cconn *c)
{
    while (c->readable) {
        uint32_t ask = cl->rbuf_cap - c->got;
        if (ask > RECV_CHUNK)
            ask = RECV_CHUNK;
        cl->n.recv++;
        ssize_t n = recv(c->fd, c->rbuf + c->got, ask, 0);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            if (errno != EAGAIN && errno != EWOULDBLOCK)
                die("recv on connection %td: %s", c - cl->conns, strerror(errno));
            cl->n.eagain_recv++;
            c->readable = false;
            return false;
        }
        if (n == 0)
            die("connection %td closed by the server", c - cl->conns);
        c->got += (uint32_t)n;
        /* Fewer bytes than asked: the socket is empty and the next arrival is a new
         * edge. A recv that was filled may have left bytes behind, so it is retried. */
        if ((uint32_t)n < ask)
            c->readable = false;
        if (c->got >= cl->rep) {
            if (c->got > cl->rep)
                die("connection %td: %u bytes for a %u-byte reply", c - cl->conns, c->got,
                    cl->rep);
            return true;
        }
        cl->n.partial_recv++;
    }
    return false;
}

static void client_run_epoll(struct client *cl)
{
    static struct epoll_event evs[EPOLL_BATCH];
    for (int i = 0; i < cl->cfg->inflight; i++)
        start_next(cl);
    while (cl->phase != PH_DONE) {
        cl->n.epoll_wait++;
        int n = epoll_wait(cl->ep, evs, EPOLL_BATCH, -1);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            die("epoll_wait: %s", strerror(errno));
        }
        for (int i = 0; i < n && cl->phase != PH_DONE; i++) {
            struct cconn *c = &cl->conns[evs[i].data.u32];
            if (evs[i].events & (EPOLLERR | EPOLLHUP | EPOLLRDHUP))
                die("connection %td: unexpected events 0x%x", c - cl->conns, evs[i].events);
            c->readable = true;
            /* A connection that kept its readiness from an earlier reply is read as
             * soon as it has a request again, which is what a tokio reader does. */
            while (c && c->busy && c->readable && epoll_recv(cl, c))
                c = reply_done(cl, c);
        }
    }
}

static void uring_completion(struct client *cl, uint64_t user_data, int32_t res)
{
    if (user_data >> 1 >= (uint64_t)cl->nconns)
        die("completion with unknown user_data %" PRIu64, user_data);
    struct cconn *c = &cl->conns[user_data >> 1];
    if (!c->busy)
        die("completion for idle connection %td", c - cl->conns);
    if (user_data & 1) {
        if (res <= 0)
            die("RECV on connection %td: %s", c - cl->conns,
                res ? strerror(-res) : "closed by the server");
        c->got += (uint32_t)res;
        if (c->got < cl->rep) {
            cl->n.partial_recv++;
            uring_queue_recv(cl, c);
            return;
        }
        if (c->got > cl->rep)
            die("connection %td: %u bytes for a %u-byte reply", c - cl->conns, c->got,
                cl->rep);
        if (c->sent == cl->req) /* never with --uring-send-inline: no SEND completes */
            cl->confirmed--;
        reply_done(cl, c);
    } else {
        if (res <= 0)
            die("SEND on connection %td: %s", c - cl->conns,
                res ? strerror(-res) : "nothing written");
        c->sent += (uint32_t)res;
        if (c->sent < cl->req) {
            cl->n.send_retry++;
            uring_queue_send(cl, c);
        } else {
            cl->confirmed++;
        }
    }
}

static void client_run_uring(struct client *cl)
{
    struct ring *r = &cl->ring;
    const bool past_sends = cl->variant == V_URING_WAIT_PAST_SENDS;
    for (int i = 0; i < cl->cfg->inflight; i++)
        start_next(cl);
    while (cl->phase != PH_DONE) {
        uint32_t head = r->cq_head;
        const uint32_t tail = __atomic_load_n(r->cq_ktail, __ATOMIC_ACQUIRE);
        if (head == tail) {
            /*
             * Nothing left to reap: one call submits the whole batch and waits.
             *
             * Waiting past the SENDs of this batch is only safe if one completion more
             * is certain to come without this thread doing anything. Every SEND
             * completes, but perhaps short, and then its reply needs the rest, which
             * only this thread can queue. A request already known to be wholly sent
             * settles it: its reply will complete a RECV. Without one (a lone request,
             * or a batch that restarted everything) a request of at most SMALL_SEND
             * bytes is taken to leave in one SEND: the socket has nothing else queued,
             * since the reply to its previous request has arrived. Larger requests then
             * wait for one completion, as the plain mode does. Were the kernel to cut a
             * small SEND short all the same, the run would stall and the alarm end it.
             */
            uint32_t min_complete = 1;
            if (past_sends && (cl->confirmed || cl->req <= SMALL_SEND))
                min_complete += r->queued_sends;
            ring_enter(cl, min_complete);
            continue;
        }
        do {
            const struct io_uring_cqe *cqe = &r->cqes[head & r->cq_mask];
            const uint64_t user_data = cqe->user_data;
            const int32_t res = cqe->res;
            r->cq_head = ++head; /* published by the next ring_enter */
            cl->n.cqes++;
            uring_completion(cl, user_data, res);
        } while (head != tail && cl->phase != PH_DONE);
    }
}

static int connect_one(uint16_t port)
{
    struct sockaddr_in addr = { .sin_family = AF_INET,
                                .sin_port = htons(port),
                                .sin_addr = { htonl(INADDR_LOOPBACK) } };
    int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0)
        die("socket: %s", strerror(errno));
    int rc;
    do
        rc = connect(fd, (struct sockaddr *)&addr, sizeof addr);
    while (rc < 0 && errno == EINTR);
    if (rc < 0)
        die("connect: %s", strerror(errno));
    set_nodelay(fd);
    /* Nonblocking in every mode. The ring does not look at O_NONBLOCK for SEND and
     * RECV: it tries once without blocking and then waits on its own poll. */
    int fl = fcntl(fd, F_GETFL);
    if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) < 0)
        die("O_NONBLOCK: %s", strerror(errno));
    return fd;
}

/* ---- reporting ---- */

static double tv_us(struct timeval tv)
{
    return (double)tv.tv_sec * 1e6 + (double)tv.tv_usec;
}

static int cmp_u32(const void *a, const void *b)
{
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return (x > y) - (x < y);
}

/* Nearest rank, in microseconds. */
static double percentile_us(const uint32_t *sorted, size_t n, double p, double tsc_hz)
{
    size_t rank = (size_t)(p * (double)n);
    if ((double)rank < p * (double)n)
        rank++;
    if (rank < 1)
        rank = 1;
    if (rank > n)
        rank = n;
    return (double)sorted[rank - 1] * 1e6 / tsc_hz;
}

static void read_line(const char *path, char *out, size_t len)
{
    FILE *f = fopen(path, "r");
    snprintf(out, len, "unknown");
    if (!f)
        return;
    if (fgets(out, (int)len, f))
        out[strcspn(out, "\n")] = 0;
    fclose(f);
}

static void report(const struct client *cl, double tsc_hz_start)
{
    const struct config *cfg = cl->cfg;
    const struct counters *n = &cl->n;
    const struct edge *open = &cl->edge[0], *close = &cl->edge[1];
    if (!n->rpcs)
        die("no request completed inside the window");
    const double rpcs = (double)n->rpcs;
    const double seconds = (double)(close->wall.ns - open->wall.ns) / 1e9;
    const double tsc_hz = tsc_hz_between(open->wall, close->wall);
    const double user = tv_us(close->ru.ru_utime) - tv_us(open->ru.ru_utime);
    const double sys = tv_us(close->ru.ru_stime) - tv_us(open->ru.ru_stime);
    const double client_clock = (double)(close->client_ns - open->client_ns) / 1e3;
    const double server = (double)(close->server_ns - open->server_ns) / 1e3;
    const double other =
        (double)(close->process_ns - open->process_ns) / 1e3 - client_clock - server;
    const double resched = (double)(close->resched - open->resched);
    const long vcsw = close->ru.ru_nvcsw - open->ru.ru_nvcsw;
    const long ivcsw = close->ru.ru_nivcsw - open->ru.ru_nivcsw;
    const size_t samples = cl->lat_seen < LAT_CAP ? (size_t)cl->lat_seen : LAT_CAP;
    qsort(cl->lat, samples, sizeof *cl->lat, cmp_u32);

    struct utsname un;
    char clocksource[64], pin[32] = "null";
    uname(&un);
    read_line("/sys/devices/system/clocksource/clocksource0/current_clocksource", clocksource,
              sizeof clocksource);
    if (cfg->pin_client >= 0)
        snprintf(pin, sizeof pin, "\"%d,%d\"", cfg->pin_client, cfg->pin_server);

    fprintf(stderr,
            "uring_ceiling: %s on Linux %s, clocksource %s, TSC %.3f MHz before the run and "
            "%.3f MHz over the window\n",
            variant_names[cfg->variant], un.release, clocksource, tsc_hz_start / 1e6,
            tsc_hz / 1e6);
    if (cfg->variant != V_EPOLL)
        fprintf(stderr,
                "uring_ceiling: ring features 0x%x, %u submission and %u completion entries, "
                "%.3f SQE and %.3f CQE per request\n",
                cl->ring.features, cl->ring.sq_entries, cl->ring.cq_entries,
                (double)n->sqes / rpcs, (double)n->cqes / rpcs);
    fprintf(stderr,
            "uring_ceiling: %.3f s, %" PRIu64 " requests; client %.0f us user + %.0f us system "
            "(CPU clock %.0f), server %.0f, other threads %.0f; client switches %ld voluntary, "
            "%ld involuntary; %.0f rescheduling interrupts on the host\n",
            seconds, n->rpcs, user, sys, client_clock, server, other, vcsw, ivcsw, resched);
    if (tsc_hz > 1.005 * tsc_hz_start || tsc_hz < 0.995 * tsc_hz_start)
        fprintf(stderr,
                "uring_ceiling: warning: the TSC rate moved by more than 0.5%%: the window "
                "length and the latency figures of this run are unreliable\n");
    if (user + sys > 1.001 * client_clock + 50 || user + sys < 0.999 * client_clock - 50)
        fprintf(stderr,
                "uring_ceiling: warning: getrusage and the thread CPU clock disagree about the "
                "client: %.0f us against %.0f us\n",
                user + sys, client_clock);
    if (n->send_retry)
        fprintf(stderr,
                "uring_ceiling: warning: %" PRIu64 " sends were cut short or had to wait, "
                "which is outside the model\n",
                n->send_retry);
    if (other > 0.01 * client_clock)
        fprintf(stderr,
                "uring_ceiling: warning: threads other than the client and the server used "
                "%.0f us: work left the client thread and its CPU figure is too low\n",
                other);
    if (cl->lat[samples - 1] == UINT32_MAX)
        fprintf(stderr, "uring_ceiling: warning: a latency sample saturated\n");

    printf("{\"mode\":\"%s\",\"variant\":\"%s\",\"conns\":%d,\"inflight\":%d,\"req\":%u,"
           "\"rep\":%u,\"work_iters\":%" PRIu64 ",\"seconds\":%.6f,\"warmup\":%.3f,\"pin\":%s,"
           "\"rpcs\":%" PRIu64 ",\"rps\":%.1f,"
           "\"client_user_us\":%.0f,\"client_sys_us\":%.0f,\"client_cpu_us\":%.0f,"
           "\"cpu_us_per_rpc\":%.4f,\"user_us_per_rpc\":%.4f,\"sys_us_per_rpc\":%.4f,"
           "\"server_cpu_us\":%.0f,\"server_cpu_us_per_rpc\":%.4f,\"other_cpu_us\":%.0f,"
           "\"send_per_rpc\":%.4f,\"recv_per_rpc\":%.4f,\"epoll_wait_per_rpc\":%.4f,"
           "\"uring_enter_per_rpc\":%.4f,\"sqe_per_rpc\":%.4f,\"cqe_per_rpc\":%.4f,"
           "\"partial_recvs\":%" PRIu64 ",\"eagain_recvs\":%" PRIu64 ",\"send_retries\":%" PRIu64
           ",\"client_vcsw\":%ld,\"client_ivcsw\":%ld,\"resched_irq_per_rpc\":%.4f,"
           "\"p50_us\":%.3f,\"p95_us\":%.3f,\"p99_us\":%.3f,\"latency_samples\":%zu,"
           "\"tsc_mhz\":%.3f,\"kernel\":\"%s\"}\n",
           cfg->variant == V_EPOLL ? "epoll" : "uring", variant_names[cfg->variant], cfg->conns,
           cfg->inflight, cfg->req, cfg->rep, cfg->work_iters, seconds, cfg->warmup, pin,
           n->rpcs, rpcs / seconds, user, sys, user + sys, (user + sys) / rpcs, user / rpcs,
           sys / rpcs, server, server / rpcs, other, (double)n->send / rpcs,
           (double)n->recv / rpcs, (double)n->epoll_wait / rpcs, (double)n->uring_enter / rpcs,
           (double)n->sqes / rpcs, (double)n->cqes / rpcs, n->partial_recv, n->eagain_recv,
           n->send_retry, vcsw, ivcsw, resched / rpcs,
           percentile_us(cl->lat, samples, 0.50, tsc_hz),
           percentile_us(cl->lat, samples, 0.95, tsc_hz),
           percentile_us(cl->lat, samples, 0.99, tsc_hz), samples, tsc_hz / 1e6, un.release);
}

/*
 * --calibrate: the cost of one work iteration, in calls of the size the run would make
 * (4096 iterations when --work-iters is 0), over a reply-sized buffer. The clock is
 * read once before and once after.
 */
static int run_calibrate(const struct config *cfg)
{
    uint64_t per_call = cfg->work_iters ? cfg->work_iters : 4096;
    if (per_call > 1u << 20)
        per_call = 1u << 20;
    uint8_t *buf = xcalloc(1, cfg->rep);
    fill_pattern(buf, cfg->rep, 89);
    const double tsc_hz = tsc_hz_initial();
    uint64_t end = __rdtsc() + (uint64_t)(0.02 * tsc_hz);
    while (__rdtsc() < end)
        g_sink = mix_work(buf, cfg->rep, per_call);
    const struct clockpair a = clock_pair();
    uint64_t calls = 0;
    end = a.tsc + (uint64_t)(0.2 * tsc_hz);
    do {
        g_sink = mix_work(buf, cfg->rep, per_call);
        calls++;
    } while (__rdtsc() < end);
    const struct clockpair b = clock_pair();
    const double ns = (double)(b.ns - a.ns);
    const double per_iter = ns / ((double)calls * (double)per_call);
    printf("{\"mode\":\"calibrate\",\"rep\":%u,\"iters_per_call\":%" PRIu64 ",\"calls\":%" PRIu64
           ",\"seconds\":%.6f,\"work_ns_per_iter\":%.4f,\"work_iters_per_us\":%.1f,"
           "\"tsc_mhz\":%.3f}\n",
           cfg->rep, per_call, calls, ns / 1e9, per_iter, 1000.0 / per_iter, tsc_hz / 1e6);
    free(buf);
    return 0;
}

/* ---- command line ---- */

static uint64_t parse_u64(const char *name, const char *text, uint64_t lo, uint64_t hi)
{
    char *end;
    errno = 0;
    unsigned long long v = strtoull(text, &end, 10);
    if (errno || end == text || *end || *text == '-' || v < lo || v > hi)
        die("--%s must be an integer from %" PRIu64 " to %" PRIu64 ", not '%s'", name, lo, hi,
            text);
    return v;
}

static double parse_seconds(const char *name, const char *text, double lo)
{
    char *end;
    errno = 0;
    double v = strtod(text, &end);
    if (errno || end == text || *end || !(v >= lo) || v > 3600)
        die("--%s must be a number of seconds from %g to 3600, not '%s'", name, lo, text);
    return v;
}

static void usage(FILE *out)
{
    fputs("usage: uring_ceiling [--mode epoll|uring] [--conns 64] [--inflight 16] [--req 270]\n"
          "         [--rep 1592] [--work-iters 0] [--seconds 3] [--warmup 0.5] [--pin C,S]\n"
          "         [--uring-send-inline | --uring-wait-past-sends] [--calibrate]\n"
          "--pin C,S               pin the client thread to cpu C and the server thread to S\n"
          "--uring-send-inline     ring mode: plain send(), only the recv on the ring\n"
          "--uring-wait-past-sends ring mode: wait for one completion beyond the SENDs that\n"
          "                        the same io_uring_enter submits\n"
          "--calibrate             print the cost of one --work-iters iteration and exit\n",
          out);
}

static void parse_args(int argc, char **argv, struct config *cfg)
{
    enum { O_MODE = 256, O_CONNS, O_INFLIGHT, O_REQ, O_REP, O_WORK, O_SECONDS, O_WARMUP, O_PIN,
           O_SEND_INLINE, O_PAST_SENDS, O_CALIBRATE, O_HELP };
    static const struct option options[] = {
        { "mode", required_argument, NULL, O_MODE },
        { "conns", required_argument, NULL, O_CONNS },
        { "inflight", required_argument, NULL, O_INFLIGHT },
        { "req", required_argument, NULL, O_REQ },
        { "rep", required_argument, NULL, O_REP },
        { "work-iters", required_argument, NULL, O_WORK },
        { "seconds", required_argument, NULL, O_SECONDS },
        { "warmup", required_argument, NULL, O_WARMUP },
        { "pin", required_argument, NULL, O_PIN },
        { "uring-send-inline", no_argument, NULL, O_SEND_INLINE },
        { "uring-wait-past-sends", no_argument, NULL, O_PAST_SENDS },
        { "calibrate", no_argument, NULL, O_CALIBRATE },
        { "help", no_argument, NULL, O_HELP },
        { NULL, 0, NULL, 0 },
    };
    bool uring = false, send_inline = false, past_sends = false;
    int opt;
    while ((opt = getopt_long(argc, argv, "", options, NULL)) != -1) {
        switch (opt) {
        case O_MODE:
            if (strcmp(optarg, "epoll") && strcmp(optarg, "uring"))
                die("--mode must be epoll or uring, not '%s'", optarg);
            uring = !strcmp(optarg, "uring");
            break;
        case O_CONNS:
            cfg->conns = (int)parse_u64("conns", optarg, 1, MAX_CONNS);
            break;
        case O_INFLIGHT:
            cfg->inflight = (int)parse_u64("inflight", optarg, 1, MAX_CONNS);
            break;
        case O_REQ:
            cfg->req = (uint32_t)parse_u64("req", optarg, TAG_BYTES, MAX_MESSAGE);
            break;
        case O_REP:
            cfg->rep = (uint32_t)parse_u64("rep", optarg, TAG_BYTES, MAX_MESSAGE);
            break;
        case O_WORK:
            cfg->work_iters = parse_u64("work-iters", optarg, 0, UINT64_MAX);
            break;
        case O_SECONDS:
            cfg->seconds = parse_seconds("seconds", optarg, 0.001);
            break;
        case O_WARMUP:
            cfg->warmup = parse_seconds("warmup", optarg, 0);
            break;
        case O_PIN: {
            char *comma = strchr(optarg, ',');
            if (!comma)
                die("--pin takes two cpus, as in --pin 0,2");
            *comma = 0;
            cfg->pin_client = (int)parse_u64("pin", optarg, 0, CPU_SETSIZE - 1);
            cfg->pin_server = (int)parse_u64("pin", comma + 1, 0, CPU_SETSIZE - 1);
            break;
        }
        case O_SEND_INLINE:
            send_inline = true;
            break;
        case O_PAST_SENDS:
            past_sends = true;
            break;
        case O_CALIBRATE:
            cfg->calibrate = true;
            break;
        case O_HELP:
            usage(stdout);
            exit(0);
        default:
            usage(stderr);
            exit(2);
        }
    }
    if (optind != argc)
        die("unexpected argument '%s'", argv[optind]);
    if (cfg->inflight > cfg->conns)
        die("--inflight %d needs at least as many --conns, not %d", cfg->inflight, cfg->conns);
    if ((send_inline || past_sends) && !uring && !cfg->calibrate)
        die("--uring-send-inline and --uring-wait-past-sends need --mode uring");
    if (send_inline && past_sends)
        die("--uring-wait-past-sends has no SEND to wait past with --uring-send-inline");
    cfg->variant = !uring ? V_EPOLL
                   : send_inline ? V_URING_SEND_INLINE
                   : past_sends ? V_URING_WAIT_PAST_SENDS
                   : V_URING;
}

static void on_alarm(int sig)
{
    static const char msg[] = "uring_ceiling: fatal: no progress, gave up\n";
    (void)sig;
    if (write(STDERR_FILENO, msg, sizeof msg - 1) < 0) {
    }
    _exit(3);
}

int main(int argc, char **argv)
{
    struct config cfg = { .variant = V_EPOLL, .conns = 64, .inflight = 16, .req = 270,
                          .rep = 1592, .work_iters = 0, .seconds = 3, .warmup = 0.5,
                          .pin_client = -1, .pin_server = -1, .calibrate = false };
    parse_args(argc, argv, &cfg);
    signal(SIGPIPE, SIG_IGN);
    if (cfg.pin_client >= 0)
        pin_self(cfg.pin_client, "client");
    if (cfg.calibrate)
        return run_calibrate(&cfg);

    /* A run that stops making progress must not hang whoever started it. */
    signal(SIGALRM, on_alarm);
    alarm((unsigned)(cfg.warmup + cfg.seconds) + 20);

    struct rlimit nofile;
    const rlim_t need = 2 * (rlim_t)cfg.conns + 32;
    if (getrlimit(RLIMIT_NOFILE, &nofile) == 0 && nofile.rlim_cur < need) {
        nofile.rlim_cur = need < nofile.rlim_max ? need : nofile.rlim_max;
        if (nofile.rlim_cur < need || setrlimit(RLIMIT_NOFILE, &nofile) < 0)
            die("%d connections need %llu file descriptors", cfg.conns,
                (unsigned long long)need);
    }

    /* Server: listen on an ephemeral loopback port, then accept on its own thread. */
    struct server srv = { .cfg = &cfg };
    struct sockaddr_in addr = { .sin_family = AF_INET, .sin_addr = { htonl(INADDR_LOOPBACK) } };
    socklen_t addr_len = sizeof addr;
    srv.listen_fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (srv.listen_fd < 0 || bind(srv.listen_fd, (struct sockaddr *)&addr, sizeof addr) < 0 ||
        listen(srv.listen_fd, cfg.conns) < 0 ||
        getsockname(srv.listen_fd, (struct sockaddr *)&addr, &addr_len) < 0)
        die("listening socket: %s", strerror(errno));
    srv.wake_fd = eventfd(0, EFD_CLOEXEC);
    if (srv.wake_fd < 0 || sem_init(&srv.ready, 0, 0) < 0)
        die("eventfd: %s", strerror(errno));
    srv.conns = xcalloc((size_t)cfg.conns, sizeof *srv.conns);
    for (int i = 0; i < cfg.conns; i++) {
        srv.conns[i].rbuf = xcalloc(1, cfg.req);
        srv.conns[i].wbuf = xcalloc(1, cfg.rep);
    }
    pthread_t server_thread;
    int rc = pthread_create(&server_thread, NULL, server_main, &srv);
    if (rc)
        die("pthread_create: %s", strerror(rc));
    clockid_t server_clock;
    rc = pthread_getcpuclockid(server_thread, &server_clock);
    if (rc)
        die("pthread_getcpuclockid: %s", strerror(rc));

    /* Client: this thread. The reply buffer holds whole RECV_CHUNKs and at least one
     * byte more than a reply, so that a reply that is too long is seen. */
    struct client cl = { .cfg = &cfg, .variant = cfg.variant, .nconns = cfg.conns,
                         .req = cfg.req, .rep = cfg.rep, .work_iters = cfg.work_iters,
                         .rbuf_cap = (cfg.rep / RECV_CHUNK + 1) * RECV_CHUNK,
                         .phase = PH_WARMUP, .rng = 0x2545F4914F6CDD1Dull, .ep = -1,
                         .wake_fd = srv.wake_fd, .server_clock = server_clock };
    cl.conns = xcalloc((size_t)cfg.conns, sizeof *cl.conns);
    cl.lat = xcalloc(LAT_CAP, sizeof *cl.lat);
    memset(cl.lat, 0xff, LAT_CAP * sizeof *cl.lat); /* touched before the window */
    if (cfg.variant == V_EPOLL) {
        cl.ep = epoll_create1(EPOLL_CLOEXEC);
        if (cl.ep < 0)
            die("epoll_create1: %s", strerror(errno));
    } else {
        /* At most two operations per request are outstanding or unreaped at any time,
         * so four entries per connection can never overflow. */
        uint32_t cq = 4 * (uint32_t)cfg.conns;
        ring_init(&cl.ring, SQ_ENTRIES, cq < 2 * SQ_ENTRIES ? 2 * SQ_ENTRIES : cq);
    }
    for (int i = 0; i < cfg.conns; i++) {
        struct cconn *c = &cl.conns[i];
        c->fd = connect_one(ntohs(addr.sin_port));
        c->sbuf = xcalloc(1, cfg.req);
        c->rbuf = xcalloc(1, cl.rbuf_cap);
        fill_pattern(c->sbuf, cfg.req, 7);
        memset(c->rbuf, 0xa5, cl.rbuf_cap);
        if (cfg.variant == V_EPOLL) {
            struct epoll_event ev = { .events = EPOLLIN | EPOLLRDHUP | EPOLLET,
                                      .data = { .u32 = (uint32_t)i } };
            if (epoll_ctl(cl.ep, EPOLL_CTL_ADD, c->fd, &ev) < 0)
                die("epoll_ctl: %s", strerror(errno));
        }
    }
    while (sem_wait(&srv.ready) < 0 && errno == EINTR) {
    }

    const double tsc_hz_start = tsc_hz_initial();
    cl.tsc_hz = tsc_hz_start;
    cl.deadline = __rdtsc() + (uint64_t)(cfg.warmup * cl.tsc_hz);
    if (cfg.variant == V_EPOLL)
        client_run_epoll(&cl);
    else
        client_run_uring(&cl);

    rc = pthread_join(server_thread, NULL);
    if (rc)
        die("pthread_join: %s", strerror(rc));
    if (cfg.variant != V_EPOLL) {
        uint32_t overflow = __atomic_load_n(cl.ring.cq_koverflow, __ATOMIC_ACQUIRE);
        uint32_t dropped = __atomic_load_n(cl.ring.sq_kdropped, __ATOMIC_ACQUIRE);
        if (overflow || dropped)
            die("the ring overflowed %u completions and dropped %u submissions", overflow,
                dropped);
    }
    report(&cl, tsc_hz_start);
    return 0;
}
