// Command runner is the native Go runner of `make bench-compare-lib`: it
// builds one library's table from a corpus, validates it against the
// oracle's expected results, then measures lookup throughput and sampled
// latencies, and prints one JSON object per line (schema in
// benchmarks/compare-lib/README.md).
//
// Every lookup loop lives in the library's own file and calls the library
// directly: the harness dispatches once per measurement, never per lookup.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"sort"
	"strconv"
	"strings"
	"sync"
	_ "unsafe" // go:linkname

	"dir248bench/compare-lib/internal/corpus"
)

// nanotime is the runtime's monotonic clock (CLOCK_MONOTONIC through the
// vDSO): one clock read, like Instant::now in Rust and clock_gettime in C.
//
//go:linkname nanotime runtime.nanotime
func nanotime() int64

const (
	opContains = 0
	opLookup   = 1
)

var opNames = [2]string{"contains", "lookup"}

// lib is one library adapter. Implementations keep their own copy of the
// queries in the library's native address type (prepared before any
// timing) and run whole loops, so that per-lookup calls are direct.
type lib interface {
	// info describes the library (versions come from the build info).
	info() map[string]any
	// build inserts every prefix; returns the number of storage units and
	// their name when the library exposes one (-1 otherwise).
	build(family int, ps []corpus.Prefix) (int64, string)
	// prepare converts a scenario's addresses to the native type.
	prepare(name string, q *corpus.Queries)
	// use selects the scenario of the next calls.
	use(name string)
	// one runs one operation on query i (validation, not timed).
	one(op, i int) (uint32, bool)
	// loop runs n operations on queries lo, lo+1, ... (index & mask);
	// returns the checksum (sum of ids, or count of true) and errors.
	loop(op, lo, n int) (uint64, uint64)
	// latency times each operation individually into samples.
	latency(op int, samples []int64) (uint64, uint64)
}

var out = bufio.NewWriterSize(os.Stdout, 1<<16)

func emit(rec map[string]any) {
	b, err := json.Marshal(rec)
	if err != nil {
		panic(err)
	}
	out.Write(b)
	out.WriteByte('\n')
	out.Flush()
}

func fail(format string, args ...any) {
	emit(map[string]any{"type": "error", "message": fmt.Sprintf(format, args...)})
	os.Exit(2)
}

type task struct {
	kind     string // throughput | latency
	op       int
	scenario string
	samples  int
	threads  int // workers of a throughput measurement
}

type plan struct {
	lib      string
	corpus   string
	targetMs int
	warmup   int
	prewarm  int // milliseconds
	edge     bool
	validate []struct {
		scenario string
		limit    int
	}
	tasks []task
}

func parseOp(s string) int {
	switch s {
	case "contains":
		return opContains
	case "lookup":
		return opLookup
	}
	fail("unknown operation %q", s)
	return 0
}

func readPlan(path string) plan {
	f, err := os.ReadFile(path)
	if err != nil {
		fail("plan: %v", err)
	}
	p := plan{targetMs: 100, warmup: 1 << 18}
	for _, line := range strings.Split(string(f), "\n") {
		w := strings.Fields(line)
		if len(w) == 0 || strings.HasPrefix(w[0], "#") {
			continue
		}
		num := func(i int) int {
			if i >= len(w) {
				fail("plan line %q: missing field", line)
			}
			v, err := strconv.Atoi(w[i])
			if err != nil {
				fail("plan line %q: %v", line, err)
			}
			return v
		}
		switch w[0] {
		case "lib":
			p.lib = w[1]
		case "corpus":
			p.corpus = w[1]
		case "target_ms":
			p.targetMs = num(1)
		case "warmup":
			p.warmup = num(1)
		case "prewarm_ms":
			p.prewarm = num(1)
		case "edge":
			p.edge = true
		case "validate":
			p.validate = append(p.validate, struct {
				scenario string
				limit    int
			}{w[1], num(2)})
		case "throughput":
			threads := 1
			if len(w) > 3 {
				threads = max(1, num(3))
			}
			p.tasks = append(p.tasks, task{kind: "throughput", op: parseOp(w[1]), scenario: w[2], threads: threads})
		case "latency":
			p.tasks = append(p.tasks, task{kind: "latency", op: parseOp(w[1]), scenario: w[2], samples: num(3), threads: 1})
		case "mmdb":
			// C runner only.
		default:
			fail("unknown plan directive %q", w[0])
		}
	}
	return p
}

// procStatus returns VmRSS and VmHWM in bytes.
func procStatus() (int64, int64) {
	b, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return -1, -1
	}
	var rss, hwm int64 = -1, -1
	for _, l := range strings.Split(string(b), "\n") {
		f := strings.Fields(l)
		if len(f) < 2 {
			continue
		}
		v, _ := strconv.ParseInt(f[1], 10, 64)
		switch f[0] {
		case "VmRSS:":
			rss = v * 1024
		case "VmHWM:":
			hwm = v * 1024
		}
	}
	return rss, hwm
}

// settle collects garbage and returns free memory to the system, so that
// the resident set size reflects live data (the same step as malloc_trim in
// the Rust and C runners).
func settle() {
	runtime.GC()
	debug.FreeOSMemory()
}

func heapAlloc() int64 {
	runtime.GC()
	var m runtime.MemStats
	runtime.ReadMemStats(&m)
	return int64(m.HeapAlloc)
}

// clockStats measures the cost and the resolution of nanotime.
func clockStats() map[string]any {
	const n = 200000
	d := make([]int64, n)
	for i := range d {
		t0 := nanotime()
		t1 := nanotime()
		d[i] = t1 - t0
	}
	sort.Slice(d, func(a, b int) bool { return d[a] < d[b] })
	res := int64(1 << 62)
	prev := nanotime()
	for i := 0; i < 2000000; i++ {
		t := nanotime()
		if t != prev {
			if t-prev < res {
				res = t - prev
			}
			prev = t
		}
	}
	return map[string]any{
		"source":        "runtime.nanotime (CLOCK_MONOTONIC, vDSO)",
		"overhead_ns":   d[n/2],
		"overhead_min":  d[0],
		"resolution_ns": res,
	}
}

func percentiles(s []int64) map[string]any {
	sort.Slice(s, func(a, b int) bool { return s[a] < s[b] })
	n := len(s)
	rank := func(p float64) int64 {
		k := int(p/100*float64(n)+0.999999999) - 1
		if k < 0 {
			k = 0
		}
		if k >= n {
			k = n - 1
		}
		return s[k]
	}
	var sum float64
	for _, v := range s {
		sum += float64(v)
	}
	m := map[string]any{
		"min": s[0], "p1": rank(1), "p5": rank(5), "p25": rank(25), "p50": rank(50), "p75": rank(75),
		"p90": rank(90), "p95": rank(95), "p99": rank(99), "max": s[n-1], "mean": sum / float64(n),
	}
	if n >= 100000 {
		m["p99_9"] = rank(99.9)
	} else {
		m["p99_9"] = nil
	}
	return m
}

func buildInfo() (map[string]any, []map[string]any) {
	bi, ok := debug.ReadBuildInfo()
	settings := map[string]any{}
	var mods []map[string]any
	if !ok {
		return settings, mods
	}
	settings["go"] = bi.GoVersion
	for _, s := range bi.Settings {
		settings[s.Key] = s.Value
	}
	for _, d := range bi.Deps {
		m := map[string]any{"path": d.Path, "version": d.Version, "sum": d.Sum}
		if d.Replace != nil {
			m["replace"] = d.Replace.Path + "@" + d.Replace.Version
		}
		mods = append(mods, m)
	}
	return settings, mods
}

// runThreads splits `lookups` operations over `threads` goroutines sharing
// the library (read only): worker k runs indices [k*lookups/threads,
// (k+1)*lookups/threads). The goroutines are started before the clock and
// released together by closing a channel; the wall time runs from the
// release to the end of the last worker.
func runThreads(l lib, op, lookups, threads int) (sum, errs uint64, wall int64) {
	if threads == 1 {
		t0 := nanotime()
		sum, errs = l.loop(op, 0, lookups)
		return sum, errs, nanotime() - t0
	}
	var ready, done sync.WaitGroup
	start := make(chan struct{})
	sums := make([]uint64, threads)
	bad := make([]uint64, threads)
	ends := make([]int64, threads)
	ready.Add(threads)
	done.Add(threads)
	for k := 0; k < threads; k++ {
		go func(k int) {
			defer done.Done()
			lo := k * lookups / threads
			hi := (k + 1) * lookups / threads
			ready.Done()
			<-start
			sums[k], bad[k] = l.loop(op, lo, hi-lo)
			ends[k] = nanotime()
		}(k)
	}
	ready.Wait()
	t0 := nanotime()
	close(start)
	done.Wait()
	last := t0
	for k := 0; k < threads; k++ {
		sum += sums[k]
		errs += bad[k]
		last = max(last, ends[k])
	}
	return sum, errs, last - t0
}

func newLib(id string) lib {
	switch id {
	case "go-cidranger-libp2p":
		return &libp2pRanger{}
	case "cidranger-yl2chen":
		return &yl2chenRanger{}
	case "go-iptrie":
		return &goIPTrie{}
	case "bart":
		return &bartTable{}
	}
	fail("unknown library %q", id)
	return nil
}

func main() {
	if len(os.Args) != 3 || os.Args[1] != "--plan" {
		fmt.Fprintln(os.Stderr, "usage: runner --plan <file>")
		os.Exit(64)
	}
	p := readPlan(os.Args[2])
	l := newLib(p.lib)
	settings, mods := buildInfo()
	oldGC := debug.SetGCPercent(100)
	debug.SetGCPercent(oldGC)
	meta := map[string]any{
		"type": "meta", "runner": "go", "lib": p.lib, "impl": l.info(), "build": settings, "modules": mods,
		"runtime": map[string]any{"GOMAXPROCS": runtime.GOMAXPROCS(0), "NumCPU": runtime.NumCPU(), "GOGC": oldGC,
			"GOAMD64": settings["GOAMD64"]},
		"clock": clockStats(),
	}
	emit(meta)

	// Queries first (they are not part of the structure's memory).
	queries := map[string]*corpus.Queries{}
	need := func(s string) {
		if _, ok := queries[s]; ok {
			return
		}
		q, err := corpus.ReadQueries(filepath.Join(p.corpus, "q-"+s+".bin"))
		if err != nil {
			fail("queries: %v", err)
		}
		queries[s] = q
		l.prepare(s, q)
	}
	for _, v := range p.validate {
		need(v.scenario)
	}
	for _, t := range p.tasks {
		need(t.scenario)
		if n := queries[t.scenario].N; n&(n-1) != 0 {
			fail("measured scenario %s: query count %d is not a power of two", t.scenario, n)
		}
	}
	family, prefixes, err := corpus.ReadPrefixes(filepath.Join(p.corpus, "prefixes.bin"))
	if err != nil {
		fail("prefixes: %v", err)
	}

	heap0 := heapAlloc()
	settle()
	rss0, _ := procStatus()
	t0 := nanotime()
	units, unitsName := l.build(family, prefixes)
	buildNs := nanotime() - t0
	heap1 := heapAlloc()
	settle()
	rss1, _ := procStatus()
	emit(map[string]any{"type": "build", "family": family, "prefixes": len(prefixes), "build_ns": buildNs,
		"rss_before": rss0, "rss_after": rss1, "heap_before": heap0, "heap_after": heap1,
		"structure_bytes": heap1 - heap0, "structure_method": "Go heap (runtime.MemStats.HeapAlloc after GC) before/after build",
		"units": units, "units_name": unitsName})

	invalid := map[int]bool{}
	for _, v := range p.validate {
		q := queries[v.scenario]
		l.use(v.scenario)
		n := q.N
		if v.limit > 0 && v.limit < n {
			n = v.limit
		}
		for op := 0; op < 2; op++ {
			var mism, errs int
			var examples []map[string]any
			for i := 0; i < n; i++ {
				got, bad := l.one(op, i)
				want := q.Expected[i]
				if op == opContains && want != 0 {
					want = 1
				}
				if bad {
					errs++
				} else if got != want {
					mism++
				} else {
					continue
				}
				if len(examples) < 5 {
					examples = append(examples, map[string]any{"addr": fmt.Sprintf("%x", q.Addr(i)), "want": want, "got": got, "error": bad})
				}
			}
			if mism > 0 || errs > 0 {
				invalid[op] = true
			}
			emit(map[string]any{"type": "validation", "scenario": v.scenario, "op": opNames[op], "checked": n,
				"mismatches": mism, "errors": errs, "examples": examples})
		}
	}

	// Warm-up: one pass over each measured scenario per operation, stopped
	// after prewarm_ms (0: no time limit).
	warmed := map[string]bool{}
	for _, t := range p.tasks {
		if warmed[t.scenario] {
			continue
		}
		warmed[t.scenario] = true
		l.use(t.scenario)
		n := queries[t.scenario].N
		for op := 0; op < 2; op++ {
			done := 0
			w0 := nanotime()
			for done < n && (p.prewarm == 0 || nanotime()-w0 < int64(p.prewarm)*1e6) {
				c := min(4096, n-done)
				l.loop(op, done, c)
				done += c
			}
			emit(map[string]any{"type": "warmup", "scenario": t.scenario, "op": opNames[op], "lookups": done})
		}
	}

	volume := map[string]int{}
	for _, t := range p.tasks {
		op := opNames[t.op]
		if invalid[t.op] {
			emit(map[string]any{"type": "skip", "task": t.kind, "op": op, "scenario": t.scenario, "threads": t.threads,
				"reason": "validation failed: results differ from the oracle"})
			continue
		}
		q := queries[t.scenario]
		l.use(t.scenario)
		mask := q.N - 1
		expect := func(lookups int) uint64 {
			var s uint64
			for i := 0; i < lookups; i++ {
				e := q.Expected[i&mask]
				if t.op == opContains {
					if e != 0 {
						s++
					}
				} else {
					s += uint64(e)
				}
			}
			return s
		}
		switch t.kind {
		case "throughput":
			// One single-thread calibration per (operation, scenario): every
			// thread count then runs the same total number of lookups.
			key := op + "/" + t.scenario
			lookups, ok := volume[key]
			if !ok {
				warm := min(p.warmup, q.N)
				runtime.GC()
				c0 := nanotime()
				l.loop(t.op, 0, warm)
				per := float64(nanotime()-c0) / float64(warm)
				lookups = min(max(int(float64(p.targetMs)*1e6/per)+1, 1<<14), 1<<27)
				lookups = (lookups + 1023) &^ 1023
				volume[key] = lookups
			}
			runtime.GC()
			sum, errs, wall := runThreads(l, t.op, lookups, t.threads)
			emit(map[string]any{"type": "throughput", "op": op, "scenario": t.scenario, "threads": t.threads,
				"lookups": lookups, "wall_ns": wall, "checksum": sum, "expected_checksum": expect(lookups), "errors": errs,
				"gomaxprocs": runtime.GOMAXPROCS(0)})
		case "latency":
			samples := make([]int64, t.samples)
			runtime.GC()
			sum, errs := l.latency(t.op, samples)
			rec := map[string]any{"type": "latency", "op": op, "scenario": t.scenario, "samples": t.samples,
				"checksum": sum, "expected_checksum": expect(t.samples), "errors": errs, "raw_ns": percentiles(samples)}
			emit(rec)
		}
	}
	if len(p.tasks) > 0 {
		// The structure as used by every measured query (memory-mapped
		// pages included), garbage returned to the system first.
		heap2 := heapAlloc()
		settle()
		rss2, _ := procStatus()
		emit(map[string]any{"type": "memory", "stage": "after_measurements", "rss": rss2, "heap": heap2})
	}
	_, hwm := procStatus()
	emit(map[string]any{"type": "end", "hwm": hwm})
	// Go's GC follows liveness: without these, the structure, the queries
	// and the input (part of the baseline) could be collected before the
	// last memory reading.
	runtime.KeepAlive(l)
	runtime.KeepAlive(queries)
	runtime.KeepAlive(prefixes)
}
