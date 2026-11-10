// LD_PRELOAD: sample backtraces of clock_gettime(CLOCK_MONOTONIC) callers. Measurement tool only.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <execinfo.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/syscall.h>
static int (*real_cg)(clockid_t, struct timespec *);
static __thread long count;
static __thread int busy;
static int out_fd = -1;
static long every = 251;
__attribute__((constructor)) static void init(void) {
  real_cg = dlsym(RTLD_NEXT, "clock_gettime");
  const char *e = getenv("CLOCKBT_EVERY"); if (e) every = atol(e);
  const char *o = getenv("CLOCKBT_OUT");
  if (o) { char p[512]; snprintf(p, sizeof p, "%s.%d", o, (int)getpid()); out_fd = open(p, O_WRONLY | O_CREAT | O_APPEND, 0644); }
  void *warm[4]; backtrace(warm, 4); // load the unwinder outside the hot path
}
int clock_gettime(clockid_t id, struct timespec *ts) {
  if (!real_cg) real_cg = dlsym(RTLD_NEXT, "clock_gettime");
  if (id == CLOCK_MONOTONIC && out_fd >= 0 && !busy && ++count % every == 0) {
    busy = 1;
    void *bt[40]; int k = backtrace(bt, 40);
    char line[1024]; int n = snprintf(line, sizeof line, "%ld", (long)syscall(SYS_gettid));
    for (int i = 0; i < k && n < 980; i++) n += snprintf(line + n, sizeof line - n, " %p", bt[i]);
    line[n++] = '\n';
    if (write(out_fd, line, n) < 0) {}
    busy = 0;
  }
  return real_cg(id, ts);
}
__attribute__((destructor)) static void fini(void) {
  if (out_fd < 0) return;
  int m = open("/proc/self/maps", O_RDONLY); char buf[8192]; ssize_t r;
  if (write(out_fd, "MAPS\n", 5) < 0) {}
  while (m >= 0 && (r = read(m, buf, sizeof buf)) > 0) if (write(out_fd, buf, r) < 0) {}
}
