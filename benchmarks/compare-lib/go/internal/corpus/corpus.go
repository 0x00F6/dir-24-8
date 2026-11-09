// Package corpus reads the language-neutral corpus files written by the
// orchestrator (`bench-compare`, libraries part, Rust). The layout is documented in
// benchmarks/compare-lib/README.md:
//
//	header (32 bytes, little endian): magic[8], u32 version = 1,
//	u32 family (4 or 6), u64 count, u32 record size, u32 reserved
//	prefixes.bin records: address (4 or 16 bytes, network order),
//	    u8 prefix length, 3 zero bytes, u32 record id (little endian, > 0)
//	q-*.bin records: address (4 or 16 bytes, network order),
//	    u32 expected record id of the longest match (0 = no match)
package corpus

import (
	"encoding/binary"
	"fmt"
	"os"
)

const (
	headerLen     = 32
	prefixMagic   = "DLPCPF01"
	queryMagic    = "DLPCQF01"
	formatVersion = 1
)

// Prefix is one input network.
type Prefix struct {
	Addr [16]byte // the first 4 bytes for IPv4
	Bits int
	ID   uint32
}

// Queries is one scenario: addresses and the oracle's expected ids.
type Queries struct {
	Family   int
	N        int
	Width    int    // 4 or 16
	Addrs    []byte // N * Width bytes, network order
	Expected []uint32
}

// Addr returns the bytes of address i.
func (q *Queries) Addr(i int) []byte { return q.Addrs[i*q.Width : (i+1)*q.Width] }

func readHeader(b []byte, magic string) (family, count, rec int, err error) {
	if len(b) < headerLen || string(b[:8]) != magic {
		return 0, 0, 0, fmt.Errorf("bad magic (want %s)", magic)
	}
	if v := binary.LittleEndian.Uint32(b[8:]); v != formatVersion {
		return 0, 0, 0, fmt.Errorf("unsupported corpus version %d", v)
	}
	family = int(binary.LittleEndian.Uint32(b[12:]))
	count = int(binary.LittleEndian.Uint64(b[16:]))
	rec = int(binary.LittleEndian.Uint32(b[24:]))
	if family != 4 && family != 6 {
		return 0, 0, 0, fmt.Errorf("bad family %d", family)
	}
	if len(b) != headerLen+count*rec {
		return 0, 0, 0, fmt.Errorf("size %d does not match %d records of %d bytes", len(b), count, rec)
	}
	return family, count, rec, nil
}

func width(family int) int {
	if family == 4 {
		return 4
	}
	return 16
}

// ReadPrefixes reads prefixes.bin.
func ReadPrefixes(path string) (int, []Prefix, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0, nil, err
	}
	family, count, rec, err := readHeader(b, prefixMagic)
	if err != nil {
		return 0, nil, fmt.Errorf("%s: %w", path, err)
	}
	w := width(family)
	if rec != w+8 {
		return 0, nil, fmt.Errorf("%s: record size %d", path, rec)
	}
	out := make([]Prefix, count)
	for i := range out {
		r := b[headerLen+i*rec:]
		copy(out[i].Addr[:w], r[:w])
		out[i].Bits = int(r[w])
		out[i].ID = binary.LittleEndian.Uint32(r[w+4:])
	}
	return family, out, nil
}

// ReadQueries reads one q-<scenario>.bin file.
func ReadQueries(path string) (*Queries, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	family, count, rec, err := readHeader(b, queryMagic)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	w := width(family)
	if rec != w+4 {
		return nil, fmt.Errorf("%s: record size %d", path, rec)
	}
	q := &Queries{Family: family, N: count, Width: w, Addrs: make([]byte, count*w), Expected: make([]uint32, count)}
	for i := 0; i < count; i++ {
		r := b[headerLen+i*rec:]
		copy(q.Addrs[i*w:], r[:w])
		q.Expected[i] = binary.LittleEndian.Uint32(r[w:])
	}
	return q, nil
}
