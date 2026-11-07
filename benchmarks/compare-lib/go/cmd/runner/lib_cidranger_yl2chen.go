// Same logic as lib_cidranger_libp2p.go: only the imported module differs.

package main

import (
	"net"

	cidranger "github.com/yl2chen/cidranger"

	"dir248bench/compare-lib/internal/corpus"
)

// Adapter of github.com/yl2chen/cidranger (NewPCTrieRanger). Contains is native; the longest-prefix
// lookup is adapted: ContainingNetworks, then the entry with the longest
// prefix, whatever the order of the returned slice (its allocation is part
// of the measured cost).

type yl2chenRangerEntry struct {
	network net.IPNet
	bits    int
	id      uint32
}

func (e *yl2chenRangerEntry) Network() net.IPNet { return e.network }

type yl2chenRanger struct {
	r    cidranger.Ranger
	sets map[string][]net.IP
	q    []net.IP
	mask int
}

func (l *yl2chenRanger) info() map[string]any {
	return map[string]any{"id": "cidranger-yl2chen", "module": "github.com/yl2chen/cidranger", "backend": "NewPCTrieRanger()",
		"implementation": pkgOf(cidranger.NewPCTrieRanger()),
		"contains":       "native Contains(net.IP)", "lookup": "adapted: ContainingNetworks(net.IP) + longest prefix"}
}

func (l *yl2chenRanger) build(family int, ps []corpus.Prefix) (int64, string) {
	l.r = cidranger.NewPCTrieRanger()
	for i := range ps {
		p := &ps[i]
		if err := l.r.Insert(&yl2chenRangerEntry{network: ipNet(family, p), bits: p.Bits, id: p.ID}); err != nil {
			fail("insert: %v", err)
		}
	}
	return int64(l.r.Len()), "entries (Len)"
}

func (l *yl2chenRanger) prepare(name string, q *corpus.Queries) {
	if l.sets == nil {
		l.sets = map[string][]net.IP{}
	}
	l.sets[name] = netIPs(q)
}

func (l *yl2chenRanger) use(name string) {
	l.q = l.sets[name]
	l.mask = len(l.q) - 1
}

func (l *yl2chenRanger) lpm(ip net.IP) (uint32, bool) {
	es, err := l.r.ContainingNetworks(ip)
	if err != nil {
		return 0, true
	}
	best, id := -1, uint32(0)
	for _, e := range es {
		if x := e.(*yl2chenRangerEntry); x.bits > best {
			best, id = x.bits, x.id
		}
	}
	return id, false
}

func (l *yl2chenRanger) one(op, i int) (uint32, bool) {
	if op == opContains {
		ok, err := l.r.Contains(l.q[i])
		if ok {
			return 1, err != nil
		}
		return 0, err != nil
	}
	return l.lpm(l.q[i])
}

func (l *yl2chenRanger) loop(op, lo, n int) (sum, errs uint64) {
	q, mask, r := l.q, l.mask, l.r
	if op == opContains {
		for i := lo; i < lo+n; i++ {
			ok, err := r.Contains(q[i&mask])
			if err != nil {
				errs++
			} else if ok {
				sum++
			}
		}
		return
	}
	for i := lo; i < lo+n; i++ {
		id, bad := l.lpm(q[i&mask])
		if bad {
			errs++
		}
		sum += uint64(id)
	}
	return
}

func (l *yl2chenRanger) latency(op int, s []int64) (sum, errs uint64) {
	q, mask, r := l.q, l.mask, l.r
	if op == opContains {
		for i := range s {
			ip := q[i&mask]
			t0 := nanotime()
			ok, err := r.Contains(ip)
			t1 := nanotime()
			s[i] = t1 - t0
			if err != nil {
				errs++
			} else if ok {
				sum++
			}
		}
		return
	}
	for i := range s {
		ip := q[i&mask]
		t0 := nanotime()
		id, bad := l.lpm(ip)
		t1 := nanotime()
		s[i] = t1 - t0
		if bad {
			errs++
		}
		sum += uint64(id)
	}
	return
}
