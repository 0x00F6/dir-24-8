package main

import (
	"net"
	"net/netip"
	"reflect"

	"dir248bench/compare-lib/internal/corpus"
)

// Address conversions, all done before any timing. IPv4 addresses are given
// to every library in their 4-byte form; IPv6 queries never fall in
// ::ffff:0:0/96 (the generator stays in 2000::/3), so IPv4-mapped forms
// never reach a table.

func netipAddrs(q *corpus.Queries) []netip.Addr {
	out := make([]netip.Addr, q.N)
	for i := range out {
		out[i] = netipAddr(q.Addr(i))
	}
	return out
}

func netipAddr(b []byte) netip.Addr {
	if len(b) == 4 {
		return netip.AddrFrom4([4]byte(b))
	}
	return netip.AddrFrom16([16]byte(b))
}

func netipPrefix(family int, p *corpus.Prefix) netip.Prefix {
	if family == 4 {
		return netip.PrefixFrom(netip.AddrFrom4([4]byte(p.Addr[:4])), p.Bits)
	}
	return netip.PrefixFrom(netip.AddrFrom16(p.Addr), p.Bits)
}

// netIPs slices one backing array (no per-address allocation, compact).
func netIPs(q *corpus.Queries) []net.IP {
	back := make([]byte, len(q.Addrs))
	copy(back, q.Addrs)
	out := make([]net.IP, q.N)
	w := q.Width
	for i := range out {
		out[i] = net.IP(back[i*w : (i+1)*w : (i+1)*w])
	}
	return out
}

func ipNet(family int, p *corpus.Prefix) net.IPNet {
	if family == 4 {
		ip := make(net.IP, 4)
		copy(ip, p.Addr[:4])
		return net.IPNet{IP: ip, Mask: net.CIDRMask(p.Bits, 32)}
	}
	ip := make(net.IP, 16)
	copy(ip, p.Addr[:])
	return net.IPNet{IP: ip, Mask: net.CIDRMask(p.Bits, 128)}
}

// pkgOf names the package that implements v (proves which module runs).
func pkgOf(v any) string {
	t := reflect.TypeOf(v)
	for t.Kind() == reflect.Pointer {
		t = t.Elem()
	}
	return t.PkgPath() + "." + t.Name()
}
