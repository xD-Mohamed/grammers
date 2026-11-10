// Local measurement only: give an unchanged benchmark a page-table VmRSS.
// It intercepts only a read-only open of /proc/self/status. Temporary storage
// is on the stack; diagnostics follow the page-table snapshot.
// cc -O2 -Wall -Wextra -Werror -shared -fPIC rss_snapshot.c -o rss_snapshot.so
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/memfd.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static int raw_open(int dir, const char *path, int flags, mode_t mode) {
    return (int)syscall(SYS_openat, dir, path, flags, mode);
}

static int read_proc(const char *path, char *out, size_t capacity) {
    int fd = raw_open(AT_FDCWD, path, O_RDONLY | O_CLOEXEC, 0);
    if (fd < 0) return -1;
    size_t used = 0;
    while (used + 1 < capacity) {
        ssize_t n = read(fd, out + used, capacity - used - 1);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) { close(fd); return -1; }
        if (!n) { out[used] = 0; close(fd); return 0; }
        used += (size_t)n;
    }
    close(fd);
    return -1;
}

static unsigned long field(const char *text, const char *name) {
    size_t len = strlen(name);
    for (const char *p = text; p && *p; ) {
        if (!strncmp(p, name, len)) return strtoul(p + len, NULL, 10);
        p = strchr(p, '\n');
        if (p) ++p;
    }
    return ULONG_MAX;
}

static int snapshot_open(int dir, const char *path, int flags, mode_t mode) {
    if (!path || strcmp(path, "/proc/self/status") ||
        (flags & ~(O_CLOEXEC | O_LARGEFILE)) != O_RDONLY)
        return raw_open(dir, path, flags, mode);

    char status[8192], maps[8192], result[16384];
    if (read_proc(path, status, sizeof status) ||
        read_proc("/proc/self/smaps_rollup", maps, sizeof maps)) goto failed;
    unsigned long rss = field(maps, "Rss:");
    char *start = strstr(status, "\nVmRSS:");
    if (rss == ULONG_MAX || !start) goto failed;
    ++start;
    char *end = strchr(start, '\n');
    if (!end) goto failed;
    size_t prefix = (size_t)(start - status);
    memcpy(result, status, prefix);
    int n = snprintf(result + prefix, sizeof result - prefix,
                     "VmRSS:\t%lu kB%s", rss, end);
    if (n < 0 || (size_t)n >= sizeof result - prefix) goto failed;
    size_t length = prefix + (size_t)n;
    int fd = (int)syscall(SYS_memfd_create, "benchmark-rss",
                          (flags & O_CLOEXEC) ? MFD_CLOEXEC : 0);
    if (fd < 0) goto failed;
    size_t written = 0;
    while (written < length) {
        ssize_t count = write(fd, result + written, length - written);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) { close(fd); goto failed; }
        written += (size_t)count;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) { close(fd); goto failed; }
    dprintf(STDERR_FILENO,
            "RSS_ACCOUNTING status_kib=%lu page_table_kib=%lu anonymous_kib=%lu "
            "private_clean_kib=%lu private_dirty_kib=%lu pss_file_kib=%lu\n",
            field(status, "VmRSS:"), rss, field(maps, "Anonymous:"),
            field(maps, "Private_Clean:"), field(maps, "Private_Dirty:"),
            field(maps, "Pss_File:"));
    return fd;
failed:
    dprintf(STDERR_FILENO, "RSS_ACCOUNTING_FAILED\n");
    return raw_open(dir, path, flags, mode);
}

static mode_t open_mode(int flags, va_list args) {
    return ((flags & O_CREAT) || ((flags & O_TMPFILE) == O_TMPFILE))
        ? va_arg(args, mode_t) : 0;
}

int open(const char *path, int flags, ...) {
    va_list args; va_start(args, flags);
    mode_t mode = open_mode(flags, args); va_end(args);
    return snapshot_open(AT_FDCWD, path, flags, mode);
}

int open64(const char *path, int flags, ...) {
    va_list args; va_start(args, flags);
    mode_t mode = open_mode(flags, args); va_end(args);
    return snapshot_open(AT_FDCWD, path, flags, mode);
}
