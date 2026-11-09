/*
 * Native C runner of the libraries part of `make bench-compare` for libmaxminddb (MaxMind's
 * official C reader). Same plan protocol and JSON-lines output as the Go
 * and Rust runners (benchmarks/compare-lib/README.md).
 *
 * The MMDB file is written beforehand from the same corpus (mmdbgen, not
 * timed here); this runner opens it (timed separately), converts every
 * query to a native sockaddr before any timing, validates the record ids
 * against the oracle, then measures MMDB_lookup_sockaddr:
 *   contains: found_entry of the lookup result;
 *   lookup:   lookup + MMDB_aget_value (empty path: the uint32 payload).
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <inttypes.h>
#include <malloc.h>
#include <maxminddb.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>

#define OP_CONTAINS 0
#define OP_LOOKUP 1
static const char *const OP_NAMES[2] = {"contains", "lookup"};
static const char *const EMPTY_PATH[] = {NULL};

static inline int64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static void fail(const char *msg, const char *detail) {
    printf("{\"type\":\"error\",\"message\":\"%s%s%s\"}\n", msg, detail ? ": " : "", detail ? detail : "");
    fflush(stdout);
    exit(2);
}

/* ------------------------------------------------------------ corpus */

typedef struct {
    char name[64];
    int family;
    size_t n;
    size_t stride; /* bytes per sockaddr */
    char *sa;      /* n sockaddr_in or sockaddr_in6, prepared before timing */
    uint8_t *addrs;
    uint32_t *expected;
} scenario_t;

static uint8_t *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) fail("cannot open", path);
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *b = malloc((size_t)n);
    if (!b || fread(b, 1, (size_t)n, f) != (size_t)n) fail("cannot read", path);
    fclose(f);
    *len = (size_t)n;
    return b;
}

static uint32_t le32(const uint8_t *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }
static uint64_t le64(const uint8_t *p) { return (uint64_t)le32(p) | (uint64_t)le32(p + 4) << 32; }

static void load_scenario(scenario_t *s, const char *dir, const char *name) {
    char path[4096];
    snprintf(path, sizeof path, "%s/q-%s.bin", dir, name);
    size_t len;
    uint8_t *b = read_file(path, &len);
    if (len < 32 || memcmp(b, "DLPCQF01", 8) != 0 || le32(b + 8) != 1) fail("bad query file", path);
    s->family = (int)le32(b + 12);
    s->n = (size_t)le64(b + 16);
    size_t rec = le32(b + 24);
    size_t w = s->family == 4 ? 4 : 16;
    if (rec != w + 4 || len != 32 + s->n * rec) fail("bad query file size", path);
    snprintf(s->name, sizeof s->name, "%s", name);
    s->stride = s->family == 4 ? sizeof(struct sockaddr_in) : sizeof(struct sockaddr_in6);
    s->sa = calloc(s->n, s->stride);
    s->addrs = malloc(s->n * w);
    s->expected = malloc(s->n * sizeof(uint32_t));
    if (!s->sa || !s->addrs || !s->expected) fail("out of memory", NULL);
    for (size_t i = 0; i < s->n; i++) {
        const uint8_t *r = b + 32 + i * rec;
        memcpy(s->addrs + i * w, r, w);
        s->expected[i] = le32(r + w);
        if (s->family == 4) {
            struct sockaddr_in *a = (struct sockaddr_in *)(s->sa + i * s->stride);
            a->sin_family = AF_INET;
            memcpy(&a->sin_addr, r, 4);
        } else {
            struct sockaddr_in6 *a = (struct sockaddr_in6 *)(s->sa + i * s->stride);
            a->sin6_family = AF_INET6;
            memcpy(&a->sin6_addr, r, 16);
        }
    }
    free(b);
}

static size_t prefix_count(const char *dir) {
    char path[4096];
    snprintf(path, sizeof path, "%s/prefixes.bin", dir);
    FILE *f = fopen(path, "rb");
    if (!f) fail("cannot open", path);
    uint8_t h[32];
    if (fread(h, 1, 32, f) != 32 || memcmp(h, "DLPCPF01", 8) != 0) fail("bad prefix file", path);
    fclose(f);
    return (size_t)le64(h + 16);
}

/* ------------------------------------------------------------ memory */

static void proc_status(long long *rss, long long *hwm) {
    malloc_trim(0); /* same step as settle() in the Go and Rust runners */
    *rss = *hwm = -1;
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return;
    char line[256];
    while (fgets(line, sizeof line, f)) {
        long long v;
        if (sscanf(line, "VmRSS: %lld", &v) == 1) *rss = v * 1024;
        if (sscanf(line, "VmHWM: %lld", &v) == 1) *hwm = v * 1024;
    }
    fclose(f);
}

/* Resident bytes of the mappings of `path` (/proc/self/smaps). */
static long long mapping_rss(const char *path) {
    FILE *f = fopen("/proc/self/smaps", "r");
    if (!f) return -1;
    char line[4352];
    int in = 0;
    long long total = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long a, b;
        if (sscanf(line, "%lx-%lx ", &a, &b) == 2) {
            size_t l = strlen(line);
            while (l && (line[l - 1] == '\n' || line[l - 1] == ' ')) line[--l] = 0;
            size_t p = strlen(path);
            in = l >= p && strcmp(line + l - p, path) == 0;
            continue;
        }
        long long v;
        if (in && sscanf(line, "Rss: %lld kB", &v) == 1) total += v * 1024;
    }
    fclose(f);
    return total;
}

/* ------------------------------------------------------------ lookups */

static MMDB_s db;

/* 0 ok (id set, 0 = no match), 1 error. */
static inline int lookup_id(const struct sockaddr *sa, uint32_t *id) {
    int err;
    MMDB_lookup_result_s r = MMDB_lookup_sockaddr(&db, sa, &err);
    if (err != MMDB_SUCCESS) return 1;
    if (!r.found_entry) {
        *id = 0;
        return 0;
    }
    MMDB_entry_data_s d;
    if (MMDB_aget_value(&r.entry, &d, EMPTY_PATH) != MMDB_SUCCESS || !d.has_data || d.type != MMDB_DATA_TYPE_UINT32) return 1;
    *id = d.uint32;
    return 0;
}

static inline int contains(const struct sockaddr *sa, uint32_t *hit) {
    int err;
    MMDB_lookup_result_s r = MMDB_lookup_sockaddr(&db, sa, &err);
    if (err != MMDB_SUCCESS) return 1;
    *hit = r.found_entry ? 1 : 0;
    return 0;
}

#define SA(s, i) ((const struct sockaddr *)((s)->sa + (size_t)(i) * (s)->stride))

static void run_loop(const scenario_t *s, int op, size_t lo, size_t n, uint64_t *sum, uint64_t *errs) {
    size_t mask = s->n - 1;
    uint64_t acc = 0, bad = 0;
    uint32_t v;
    if (op == OP_CONTAINS) {
        for (size_t i = lo; i < lo + n; i++) {
            if (contains(SA(s, i & mask), &v)) bad++;
            else acc += v;
        }
    } else {
        for (size_t i = lo; i < lo + n; i++) {
            if (lookup_id(SA(s, i & mask), &v)) bad++;
            else acc += v;
        }
    }
    *sum = acc;
    *errs = bad;
}

static void run_latency(const scenario_t *s, int op, int64_t *samples, size_t n, uint64_t *sum, uint64_t *errs) {
    size_t mask = s->n - 1;
    uint64_t acc = 0, bad = 0;
    uint32_t v;
    for (size_t i = 0; i < n; i++) {
        const struct sockaddr *sa = SA(s, i & mask);
        int64_t t0 = now_ns();
        int e = op == OP_CONTAINS ? contains(sa, &v) : lookup_id(sa, &v);
        int64_t t1 = now_ns();
        samples[i] = t1 - t0;
        if (e) bad++;
        else acc += v;
    }
    *sum = acc;
    *errs = bad;
}

static int cmp64(const void *a, const void *b) {
    int64_t x = *(const int64_t *)a, y = *(const int64_t *)b;
    return x < y ? -1 : x > y;
}

static int64_t rank(const int64_t *s, size_t n, double p) {
    long k = (long)(p / 100.0 * (double)n + 0.999999999) - 1;
    if (k < 0) k = 0;
    if ((size_t)k >= n) k = (long)n - 1;
    return s[k];
}

static void print_percentiles(int64_t *s, size_t n) {
    qsort(s, n, sizeof *s, cmp64);
    double sum = 0;
    for (size_t i = 0; i < n; i++) sum += (double)s[i];
    printf("\"raw_ns\":{\"min\":%" PRId64 ",\"p1\":%" PRId64 ",\"p5\":%" PRId64 ",\"p25\":%" PRId64 ",\"p50\":%" PRId64
           ",\"p75\":%" PRId64 ",\"p90\":%" PRId64 ",\"p95\":%" PRId64 ",\"p99\":%" PRId64 ",\"max\":%" PRId64 ",\"mean\":%.3f,",
           s[0], rank(s, n, 1), rank(s, n, 5), rank(s, n, 25), rank(s, n, 50), rank(s, n, 75), rank(s, n, 90),
           rank(s, n, 95), rank(s, n, 99), s[n - 1], sum / (double)n);
    if (n >= 100000) printf("\"p99_9\":%" PRId64 "}", rank(s, n, 99.9));
    else printf("\"p99_9\":null}");
}

static void clock_stats(void) {
    enum { N = 200000 };
    static int64_t d[N];
    for (int i = 0; i < N; i++) {
        int64_t t0 = now_ns();
        int64_t t1 = now_ns();
        d[i] = t1 - t0;
    }
    qsort(d, N, sizeof d[0], cmp64);
    int64_t res = INT64_MAX, prev = now_ns();
    for (int i = 0; i < 2000000; i++) {
        int64_t t = now_ns();
        if (t != prev) {
            if (t - prev < res) res = t - prev;
            prev = t;
        }
    }
    printf("\"clock\":{\"source\":\"clock_gettime(CLOCK_MONOTONIC) (vDSO)\",\"overhead_ns\":%" PRId64
           ",\"overhead_min\":%" PRId64 ",\"resolution_ns\":%" PRId64 "}",
           d[N / 2], d[0], res);
}

/* ------------------------------------------------------------ plan */

#define MAX_ITEMS 64
typedef struct {
    int kind; /* 0 throughput, 1 latency */
    int op;
    char scenario[64];
    size_t samples;
    int threads; /* workers of a throughput measurement */
} task_t;

/* ------------------------------------------------------------ threads */

typedef struct {
    const scenario_t *s;
    int op;
    size_t lo, n;
    uint64_t sum, errs;
    int64_t end;
    atomic_int *ready;
    atomic_int *go;
} worker_t;

static void *worker(void *arg) {
    worker_t *w = arg;
    atomic_fetch_add(w->ready, 1);
    while (!atomic_load_explicit(w->go, memory_order_acquire)) {
    }
    run_loop(w->s, w->op, w->lo, w->n, &w->sum, &w->errs);
    w->end = now_ns();
    return NULL;
}

/* `lookups` operations split over `threads` workers sharing the database
 * (read only): worker k runs indices [k*lookups/threads, (k+1)*lookups/threads).
 * Workers are created before the clock and released together; the wall
 * time runs from the release to the end of the last worker. */
static int64_t run_threads(const scenario_t *s, int op, size_t lookups, int threads, uint64_t *sum, uint64_t *errs) {
    if (threads <= 1) {
        int64_t t0 = now_ns();
        run_loop(s, op, 0, lookups, sum, errs);
        return now_ns() - t0;
    }
    atomic_int ready = 0, go = 0;
    worker_t *w = calloc((size_t)threads, sizeof *w);
    pthread_t *tid = calloc((size_t)threads, sizeof *tid);
    if (!w || !tid) fail("out of memory", NULL);
    for (int k = 0; k < threads; k++) {
        size_t lo = (size_t)k * lookups / (size_t)threads, hi = (size_t)(k + 1) * lookups / (size_t)threads;
        w[k] = (worker_t){s, op, lo, hi - lo, 0, 0, 0, &ready, &go};
        if (pthread_create(&tid[k], NULL, worker, &w[k])) fail("pthread_create", NULL);
    }
    while (atomic_load(&ready) < threads) {
    }
    int64_t t0 = now_ns();
    atomic_store_explicit(&go, 1, memory_order_release);
    int64_t last = t0;
    *sum = *errs = 0;
    for (int k = 0; k < threads; k++) {
        pthread_join(tid[k], NULL);
        *sum += w[k].sum;
        *errs += w[k].errs;
        if (w[k].end > last) last = w[k].end;
    }
    free(w);
    free(tid);
    return last - t0;
}

static int parse_op(const char *s) {
    if (!strcmp(s, "contains")) return OP_CONTAINS;
    if (!strcmp(s, "lookup")) return OP_LOOKUP;
    fail("unknown operation", s);
    return 0;
}

static scenario_t scen[MAX_ITEMS];
static int nscen;

static scenario_t *get_scenario(const char *dir, const char *name) {
    for (int i = 0; i < nscen; i++)
        if (!strcmp(scen[i].name, name)) return &scen[i];
    if (nscen == MAX_ITEMS) fail("too many scenarios", NULL);
    load_scenario(&scen[nscen], dir, name);
    return &scen[nscen++];
}

int main(int argc, char **argv) {
    if (argc != 3 || strcmp(argv[1], "--plan")) {
        fprintf(stderr, "usage: runner --plan <file>\n");
        return 64;
    }
    FILE *pf = fopen(argv[2], "r");
    if (!pf) fail("cannot open plan", argv[2]);
    char dir[4096] = "", mmdb[4096] = "", lib[64] = "";
    long target_ms = 100, warmup = 1 << 18, prewarm = 0;
    int edge = 0;
    char vscen[MAX_ITEMS][64];
    long vlimit[MAX_ITEMS];
    int nval = 0, ntask = 0;
    task_t tasks[MAX_ITEMS];
    char line[8192];
    while (fgets(line, sizeof line, pf)) {
        char a[4096], b[64], c[64];
        long n;
        if (line[0] == '#' || sscanf(line, "%4095s", a) != 1) continue;
        if (sscanf(line, "lib %63s", lib) == 1) continue;
        if (sscanf(line, "corpus %4095s", dir) == 1) continue;
        if (sscanf(line, "mmdb %4095s", mmdb) == 1) continue;
        if (sscanf(line, "target_ms %ld", &target_ms) == 1) continue;
        if (sscanf(line, "warmup %ld", &warmup) == 1) continue;
        if (sscanf(line, "prewarm_ms %ld", &prewarm) == 1) continue;
        if (!strcmp(a, "edge")) {
            edge = 1;
            continue;
        }
        if (nval < MAX_ITEMS && sscanf(line, "validate %63s %ld", b, &n) == 2) {
            snprintf(vscen[nval], 64, "%s", b);
            vlimit[nval++] = n;
            continue;
        }
        int nf;
        if (ntask < MAX_ITEMS && (nf = sscanf(line, "throughput %63s %63s %ld", b, c, &n)) >= 2) {
            tasks[ntask] = (task_t){0, parse_op(b), "", 0, nf == 3 && n > 1 ? (int)n : 1};
            snprintf(tasks[ntask++].scenario, 64, "%s", c);
            continue;
        }
        if (ntask < MAX_ITEMS && sscanf(line, "latency %63s %63s %ld", b, c, &n) == 3) {
            tasks[ntask] = (task_t){1, parse_op(b), "", (size_t)n, 1};
            snprintf(tasks[ntask++].scenario, 64, "%s", c);
            continue;
        }
        fail("bad plan line", a);
    }
    fclose(pf);
    if (!*dir || !*mmdb) fail("plan needs corpus and mmdb", NULL);

    printf("{\"type\":\"meta\",\"runner\":\"c\",\"lib\":\"%s\",\"impl\":{\"id\":\"libmaxminddb\",\"library_version\":\"%s\","
           "\"contains\":\"MMDB_lookup_sockaddr + found_entry\",\"lookup\":\"MMDB_lookup_sockaddr + MMDB_aget_value (uint32 payload)\","
           "\"mode\":\"MMDB_MODE_MMAP\"},\"build\":{\"compiler\":\"%s\",\"cflags\":\"%s\"},",
           lib, MMDB_lib_version(), __VERSION__, RUNNER_CFLAGS);
    clock_stats();
    printf("}\n");
    fflush(stdout);

    for (int i = 0; i < nval; i++) get_scenario(dir, vscen[i]);
    for (int i = 0; i < ntask; i++) get_scenario(dir, tasks[i].scenario);
    for (int i = 0; i < ntask; i++) {
        scenario_t *s = get_scenario(dir, tasks[i].scenario);
        if (s->n & (s->n - 1)) fail("measured scenario: query count is not a power of two", s->name);
    }
    (void)edge;
    size_t prefixes = prefix_count(dir);

    long long rss0, rss1, hwm;
    proc_status(&rss0, &hwm);
    int64_t t0 = now_ns();
    int st = MMDB_open(mmdb, MMDB_MODE_MMAP, &db);
    int64_t open_ns = now_ns() - t0;
    if (st != MMDB_SUCCESS) fail("MMDB_open", MMDB_strerror(st));
    proc_status(&rss1, &hwm);
    printf("{\"type\":\"build\",\"family\":%d,\"prefixes\":%zu,\"build_ns\":%" PRId64 ",\"open_ns\":%" PRId64
           ",\"rss_before\":%lld,\"rss_after\":%lld,\"structure_bytes\":%zu,"
           "\"structure_method\":\"MMDB file size (memory-mapped; resident share reported after warm-up)\","
           "\"units\":%u,\"units_name\":\"search tree nodes\",\"mmdb\":{\"file_bytes\":%zu,\"record_size\":%u,"
           "\"ip_version\":%u,\"node_count\":%u,\"database_type\":\"%s\"}}\n",
           scen[0].family, prefixes, open_ns, open_ns, rss0, rss1, (size_t)db.file_size, db.metadata.node_count,
           (size_t)db.file_size, db.metadata.record_size, db.metadata.ip_version, db.metadata.node_count,
           db.metadata.database_type);
    fflush(stdout);

    int invalid[2] = {0, 0};
    for (int v = 0; v < nval; v++) {
        scenario_t *s = get_scenario(dir, vscen[v]);
        size_t n = vlimit[v] > 0 && (size_t)vlimit[v] < s->n ? (size_t)vlimit[v] : s->n;
        for (int op = 0; op < 2; op++) {
            size_t mism = 0, errs = 0, shown = 0;
            char ex[2048] = "";
            for (size_t i = 0; i < n; i++) {
                uint32_t got = 0, want = s->expected[i];
                if (op == OP_CONTAINS && want) want = 1;
                int bad = op == OP_CONTAINS ? contains(SA(s, i), &got) : lookup_id(SA(s, i), &got);
                if (bad) errs++;
                else if (got != want) mism++;
                else continue;
                if (shown++ < 5) {
                    size_t w = s->family == 4 ? 4 : 16;
                    char hex[40] = "";
                    for (size_t k = 0; k < w; k++) sprintf(hex + 2 * k, "%02x", s->addrs[i * w + k]);
                    size_t l = strlen(ex);
                    snprintf(ex + l, sizeof ex - l, "%s{\"addr\":\"%s\",\"want\":%u,\"got\":%u,\"error\":%s}", l ? "," : "", hex,
                             want, got, bad ? "true" : "false");
                }
            }
            if (mism || errs) invalid[op] = 1;
            printf("{\"type\":\"validation\",\"scenario\":\"%s\",\"op\":\"%s\",\"checked\":%zu,\"mismatches\":%zu,"
                   "\"errors\":%zu,\"examples\":[%s]}\n",
                   s->name, OP_NAMES[op], n, mism, errs, ex);
            fflush(stdout);
        }
    }

    /* Warm-up: one pass over each measured scenario per operation, stopped
     * after prewarm_ms (0: no time limit). */
    for (int i = 0; i < ntask; i++) {
        scenario_t *s = get_scenario(dir, tasks[i].scenario);
        int seen = 0;
        for (int j = 0; j < i; j++) seen |= !strcmp(tasks[j].scenario, tasks[i].scenario);
        if (seen) continue;
        for (int op = 0; op < 2; op++) {
            size_t done = 0;
            int64_t w0 = now_ns();
            while (done < s->n && (prewarm == 0 || now_ns() - w0 < (int64_t)prewarm * 1000000)) {
                size_t c = s->n - done < 4096 ? s->n - done : 4096;
                uint64_t sum, errs;
                run_loop(s, op, done, c, &sum, &errs);
                done += c;
            }
            printf("{\"type\":\"warmup\",\"scenario\":\"%s\",\"op\":\"%s\",\"lookups\":%zu}\n", s->name, OP_NAMES[op], done);
        }
    }

    size_t volume[MAX_ITEMS] = {0};
    for (int t = 0; t < ntask; t++) {
        task_t *k = &tasks[t];
        scenario_t *s = get_scenario(dir, k->scenario);
        const char *op = OP_NAMES[k->op];
        const char *kind = k->kind ? "latency" : "throughput";
        if (invalid[k->op]) {
            printf("{\"type\":\"skip\",\"task\":\"%s\",\"op\":\"%s\",\"scenario\":\"%s\",\"threads\":%d,\"reason\":\"validation "
                   "failed: results differ from the oracle\"}\n",
                   kind, op, s->name, k->threads);
            fflush(stdout);
            continue;
        }
        size_t mask = s->n - 1;
        size_t count = k->samples;
        uint64_t sum = 0, errs = 0, want = 0;
        if (k->kind == 0) {
            /* One single-thread calibration per (operation, scenario):
             * every thread count then runs the same total number of lookups. */
            count = 0;
            for (int j = 0; j < t; j++)
                if (tasks[j].kind == 0 && tasks[j].op == k->op && !strcmp(tasks[j].scenario, k->scenario)) count = volume[j];
            if (!count) {
                size_t warm = (size_t)warmup < s->n ? (size_t)warmup : s->n;
                int64_t c0 = now_ns();
                run_loop(s, k->op, 0, warm, &sum, &errs);
                double per = (double)(now_ns() - c0) / (double)warm;
                double target = (double)target_ms * 1e6 / per + 1;
                count = target < 16384 ? 16384 : target > (double)(1 << 27) ? (size_t)1 << 27 : (size_t)target;
                count = (count + 1023) & ~(size_t)1023;
            }
            volume[t] = count;
            int64_t wall = run_threads(s, k->op, count, k->threads, &sum, &errs);
            for (size_t i = 0; i < count; i++) want += k->op == OP_CONTAINS ? s->expected[i & mask] != 0 : s->expected[i & mask];
            printf("{\"type\":\"throughput\",\"op\":\"%s\",\"scenario\":\"%s\",\"threads\":%d,\"lookups\":%zu,\"wall_ns\":%" PRId64
                   ",\"checksum\":%" PRIu64 ",\"expected_checksum\":%" PRIu64 ",\"errors\":%" PRIu64 "}\n",
                   op, s->name, k->threads, count, wall, sum, want, errs);
        } else {
            int64_t *samples = malloc(count * sizeof *samples);
            if (!samples) fail("out of memory", NULL);
            run_latency(s, k->op, samples, count, &sum, &errs);
            for (size_t i = 0; i < count; i++) want += k->op == OP_CONTAINS ? s->expected[i & mask] != 0 : s->expected[i & mask];
            printf("{\"type\":\"latency\",\"op\":\"%s\",\"scenario\":\"%s\",\"samples\":%zu,\"checksum\":%" PRIu64
                   ",\"expected_checksum\":%" PRIu64 ",\"errors\":%" PRIu64 ",",
                   op, s->name, count, sum, want, errs);
            print_percentiles(samples, count);
            printf("}\n");
            free(samples);
        }
        fflush(stdout);
    }
    if (ntask) {
        /* The mapping's resident share tells which pages the measured
         * lookups touched. */
        long long rss2;
        proc_status(&rss2, &hwm);
        printf("{\"type\":\"memory\",\"stage\":\"after_measurements\",\"rss\":%lld,\"mmdb_resident\":%lld,\"mmdb_file\":%zu}\n", rss2,
               mapping_rss(mmdb), (size_t)db.file_size);
    }
    long long rss3;
    proc_status(&rss3, &hwm);
    printf("{\"type\":\"end\",\"hwm\":%lld}\n", hwm);
    MMDB_close(&db);
    return 0;
}
