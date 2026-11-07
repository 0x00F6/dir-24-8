package main

import (
	"net/netip"

	"github.com/gaissmai/bart"

	"dir248bench/compare-lib/internal/corpus"
)

// Adapter of github.com/gaissmai/bart: Table[uint32] (values), native
// Contains and Lookup (longest-prefix match).

type bartTable struct {
	t    *bart.Table[uint32]
	sets map[string][]netip.Addr
	q    []netip.Addr
	mask int
}

func (l *bartTable) info() map[string]any {
	return map[string]any{"id": "bart", "module": "github.com/gaissmai/bart", "backend": "Table[uint32] (multibit trie, stride 8, popcount-compressed)",
		"implementation": pkgOf(new(bart.Table[uint32])), "contains": "native Contains(netip.Addr)", "lookup": "native Lookup(netip.Addr)"}
}

func (l *bartTable) build(family int, ps []corpus.Prefix) (int64, string) {
	l.t = new(bart.Table[uint32])
	for i := range ps {
		l.t.Insert(netipPrefix(family, &ps[i]), ps[i].ID)
	}
	return int64(l.t.Size()), "prefixes (Size)"
}

func (l *bartTable) prepare(name string, q *corpus.Queries) {
	if l.sets == nil {
		l.sets = map[string][]netip.Addr{}
	}
	l.sets[name] = netipAddrs(q)
}

func (l *bartTable) use(name string) {
	l.q = l.sets[name]
	l.mask = len(l.q) - 1
}

func (l *bartTable) one(op, i int) (uint32, bool) {
	if op == opContains {
		if l.t.Contains(l.q[i]) {
			return 1, false
		}
		return 0, false
	}
	v, _ := l.t.Lookup(l.q[i])
	return v, false
}

func (l *bartTable) loop(op, lo, n int) (sum, errs uint64) {
	q, mask, t := l.q, l.mask, l.t
	if op == opContains {
		for i := lo; i < lo+n; i++ {
			if t.Contains(q[i&mask]) {
				sum++
			}
		}
		return
	}
	for i := lo; i < lo+n; i++ {
		if v, ok := t.Lookup(q[i&mask]); ok {
			sum += uint64(v)
		}
	}
	return
}

func (l *bartTable) latency(op int, s []int64) (sum, errs uint64) {
	q, mask, t := l.q, l.mask, l.t
	if op == opContains {
		for i := range s {
			a := q[i&mask]
			t0 := nanotime()
			ok := t.Contains(a)
			t1 := nanotime()
			s[i] = t1 - t0
			if ok {
				sum++
			}
		}
		return
	}
	for i := range s {
		a := q[i&mask]
		t0 := nanotime()
		v, ok := t.Lookup(a)
		t1 := nanotime()
		s[i] = t1 - t0
		if ok {
			sum += uint64(v)
		}
	}
	return
}
