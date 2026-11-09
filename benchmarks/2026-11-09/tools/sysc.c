// LD_PRELOAD syscall-wrapper counter, per thread. Measurement tool only.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/epoll.h>
#include <sys/uio.h>
#include <sys/syscall.h>
#include <pthread.h>
enum { C_MONO, C_REAL, C_OTHERCLK, C_SEND, C_RECV, C_EPOLL, C_GETRANDOM, C_READ, C_WRITE, C_WRITEV, C_READV, C_SYSCALL, C_FUTEXLIKE, C_SENDMSG, C_RECVMSG, C_N };
static const char *names[C_N] = {"clock_mono","clock_real","clock_other","send","recv","epoll_wait","getrandom","read","write","writev","readv","syscall()","pthread_cond/futex","sendmsg","recvmsg"};
struct tc { long c[C_N]; long tid; char name[32]; struct tc *next; };
static struct tc *head; static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static __thread struct tc *mine;
static struct tc *get(void){
  if(!mine){ struct tc *t = calloc(1,sizeof *t); t->tid = syscall(SYS_gettid); pthread_mutex_lock(&mu); t->next=head; head=t; pthread_mutex_unlock(&mu); mine=t; }
  return mine; }
#define REAL(ret,name,...) static ret (*real_##name)(__VA_ARGS__); if(!real_##name) real_##name = dlsym(RTLD_NEXT,#name);
int clock_gettime(clockid_t id, struct timespec *ts){ REAL(int,clock_gettime,clockid_t,struct timespec*) get()->c[id==CLOCK_MONOTONIC?C_MONO:id==CLOCK_REALTIME?C_REAL:C_OTHERCLK]++; return real_clock_gettime(id,ts); }
ssize_t send(int fd,const void*b,size_t n,int f){ REAL(ssize_t,send,int,const void*,size_t,int) get()->c[C_SEND]++; return real_send(fd,b,n,f);} 
ssize_t recv(int fd,void*b,size_t n,int f){ REAL(ssize_t,recv,int,void*,size_t,int) get()->c[C_RECV]++; return real_recv(fd,b,n,f);} 
ssize_t sendmsg(int fd,const struct msghdr*m,int f){ REAL(ssize_t,sendmsg,int,const struct msghdr*,int) get()->c[C_SENDMSG]++; return real_sendmsg(fd,m,f);} 
ssize_t recvmsg(int fd,struct msghdr*m,int f){ REAL(ssize_t,recvmsg,int,struct msghdr*,int) get()->c[C_RECVMSG]++; return real_recvmsg(fd,m,f);} 
int epoll_wait(int e,struct epoll_event*ev,int n,int t){ REAL(int,epoll_wait,int,struct epoll_event*,int,int) get()->c[C_EPOLL]++; return real_epoll_wait(e,ev,n,t);} 
ssize_t getrandom(void*b,size_t n,unsigned f){ REAL(ssize_t,getrandom,void*,size_t,unsigned) get()->c[C_GETRANDOM]++; return real_getrandom(b,n,f);} 
ssize_t read(int fd,void*b,size_t n){ REAL(ssize_t,read,int,void*,size_t) get()->c[C_READ]++; return real_read(fd,b,n);} 
ssize_t write(int fd,const void*b,size_t n){ REAL(ssize_t,write,int,const void*,size_t) get()->c[C_WRITE]++; return real_write(fd,b,n);} 
ssize_t writev(int fd,const struct iovec*v,int n){ REAL(ssize_t,writev,int,const struct iovec*,int) get()->c[C_WRITEV]++; return real_writev(fd,v,n);} 
ssize_t readv(int fd,const struct iovec*v,int n){ REAL(ssize_t,readv,int,const struct iovec*,int) get()->c[C_READV]++; return real_readv(fd,v,n);} 
int pthread_cond_signal(pthread_cond_t*c){ REAL(int,pthread_cond_signal,pthread_cond_t*) get()->c[C_FUTEXLIKE]++; return real_pthread_cond_signal(c);} 
__attribute__((destructor)) static void report(void){
  const char *out = getenv("SYSC_OUT"); FILE *f = out? fopen(out,"a"): stderr; if(!f) return;
  for(struct tc *t=head;t;t=t->next){ long tot=0; for(int i=0;i<C_N;i++) tot+=t->c[i]; if(tot<200) continue;
    char comm[64]="?"; char p[64]; snprintf(p,sizeof p,"/proc/self/task/%ld/comm",t->tid); FILE *cf=fopen(p,"r"); if(cf){ if(fgets(comm,sizeof comm,cf)) comm[strcspn(comm,"\n")]=0; fclose(cf);} 
    fprintf(f,"SYSC tid=%ld comm=%s",t->tid,comm); for(int i=0;i<C_N;i++) if(t->c[i]) fprintf(f," %s=%ld",names[i],t->c[i]); fprintf(f,"\n"); }
  if(out) fclose(f);
}
