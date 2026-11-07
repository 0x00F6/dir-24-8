package main

import (
	"net/netip"

	"github.com/phemmer/go-iptrie"

	"dir248bench/compare-lib/internal/corpus"
)

// Adapter of github.com/phemmer/go-iptrie: native Contains and Find (value
// of the most specific network). The library stores IPv4 as IPv4-mapped
// IPv6 internally and converts each IPv4 query itself (inside the measured
// call). Values are stored as `any` (boxed uint32).

type goIPTrie struct {
	t    *iptrie.Trie
	sets map[string][]netip.Addr
	q    []netip.Addr
	mask int
}

func (l *goIPTrie) info() map[string]any {
	return map[string]any{"id": "go-iptrie", "module": "github.com/phemmer/go-iptrie", "backend": "NewTrie() (path-compressed binary trie)",
		"implementation": pkgOf(iptrie.NewTrie()), "contains": "native Contains(netip.Addr)", "lookup": "native Find(netip.Addr) (most specific network)"}
}

func (l *goIPTrie) build(family int, ps []corpus.Prefix) (int64, string) {
	l.t = iptrie.NewTrie()
	for i := range ps {
		l.t.Insert(netipPrefix(family, &ps[i]), ps[i].ID)
	}
	return -1, ""
}

func (l *goIPTrie) prepare(name string, q *corpus.Queries) {
	if l.sets == nil {
		l.sets = map[string][]netip.Addr{}
	}
	l.sets[name] = netipAddrs(q)
}

func (l *goIPTrie) use(name string) {
	l.q = l.sets[name]
	l.mask = len(l.q) - 1
}

func (l *goIPTrie) one(op, i int) (uint32, bool) {
	if op == opContains {
		if l.t.Contains(l.q[i]) {
			return 1, false
		}
		return 0, false
	}
	if v := l.t.Find(l.q[i]); v != nil {
		return v.(uint32), false
	}
	return 0, false
}

func (l *goIPTrie) loop(op, lo, n int) (sum, errs uint64) {
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
		if v := t.Find(q[i&mask]); v != nil {
			sum += uint64(v.(uint32))
		}
	}
	return
}

func (l *goIPTrie) latency(op int, s []int64) (sum, errs uint64) {
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
		v := t.Find(a)
		t1 := nanotime()
		s[i] = t1 - t0
		if v != nil {
			sum += uint64(v.(uint32))
		}
	}
	return
}
