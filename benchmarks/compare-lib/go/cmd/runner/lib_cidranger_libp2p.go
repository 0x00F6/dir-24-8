// Same logic as lib_cidranger_yl2chen.go: only the imported module differs.

package main

import (
	"net"

	cidranger "github.com/libp2p/go-cidranger"

	"dir248bench/compare-lib/internal/corpus"
)

// Adapter of github.com/libp2p/go-cidranger (NewPCTrieRanger). Contains is native; the longest-prefix
// lookup is adapted: ContainingNetworks, then the entry with the longest
// prefix, whatever the order of the returned slice (its allocation is part
// of the measured cost).

type libp2pRangerEntry struct {
	network net.IPNet
	bits    int
	id      uint32
}

func (e *libp2pRangerEntry) Network() net.IPNet { return e.network }

type libp2pRanger struct {
	r    cidranger.Ranger
	sets map[string][]net.IP
	q    []net.IP
	mask int
}

func (l *libp2pRanger) info() map[string]any {
	return map[string]any{"id": "go-cidranger-libp2p", "module": "github.com/libp2p/go-cidranger", "backend": "NewPCTrieRanger()",
		"implementation": pkgOf(cidranger.NewPCTrieRanger()),
		"contains":       "native Contains(net.IP)", "lookup": "adapted: ContainingNetworks(net.IP) + longest prefix"}
}

func (l *libp2pRanger) build(family int, ps []corpus.Prefix) (int64, string) {
	l.r = cidranger.NewPCTrieRanger()
	for i := range ps {
		p := &ps[i]
		if err := l.r.Insert(&libp2pRangerEntry{network: ipNet(family, p), bits: p.Bits, id: p.ID}); err != nil {
			fail("insert: %v", err)
		}
	}
	return int64(l.r.Len()), "entries (Len)"
}

func (l *libp2pRanger) prepare(name string, q *corpus.Queries) {
	if l.sets == nil {
		l.sets = map[string][]net.IP{}
	}
	l.sets[name] = netIPs(q)
}

func (l *libp2pRanger) use(name string) {
	l.q = l.sets[name]
	l.mask = len(l.q) - 1
}

func (l *libp2pRanger) lpm(ip net.IP) (uint32, bool) {
	es, err := l.r.ContainingNetworks(ip)
	if err != nil {
		return 0, true
	}
	best, id := -1, uint32(0)
	for _, e := range es {
		if x := e.(*libp2pRangerEntry); x.bits > best {
			best, id = x.bits, x.id
		}
	}
	return id, false
}

func (l *libp2pRanger) one(op, i int) (uint32, bool) {
	if op == opContains {
		ok, err := l.r.Contains(l.q[i])
		if ok {
			return 1, err != nil
		}
		return 0, err != nil
	}
	return l.lpm(l.q[i])
}

func (l *libp2pRanger) loop(op, lo, n int) (sum, errs uint64) {
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

func (l *libp2pRanger) latency(op int, s []int64) (sum, errs uint64) {
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
