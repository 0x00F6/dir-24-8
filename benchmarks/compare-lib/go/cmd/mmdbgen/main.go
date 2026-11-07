// Command mmdbgen writes the MMDB file that libmaxminddb reads, from the
// same corpus (CIDR -> record id) as every other library. It is a
// preparation tool (MaxMind's official Go writer), not a competitor.
//
// Prefixes are inserted by increasing length (stable, so a duplicate CIDR
// keeps the last id like the other libraries): the writer replaces
// everything inside an inserted network, so inserting the more specific
// networks last preserves the longest-prefix priority. The payload of each
// network is a single uint32, the record id.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"net"
	"os"
	"runtime/debug"
	"sort"
	"time"

	"github.com/maxmind/mmdbwriter"
	"github.com/maxmind/mmdbwriter/mmdbtype"

	"dir248bench/compare-lib/internal/corpus"
)

func main() {
	dir := flag.String("corpus", "", "corpus directory (prefixes.bin)")
	outPath := flag.String("out", "", "MMDB file to write")
	recordSize := flag.Int("record-size", 32, "MMDB record size (24, 28 or 32 bits)")
	flag.Parse()
	if *dir == "" || *outPath == "" {
		flag.Usage()
		os.Exit(64)
	}
	family, ps, err := corpus.ReadPrefixes(*dir + "/prefixes.bin")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	t0 := time.Now()
	tree, err := mmdbwriter.New(mmdbwriter.Options{
		DatabaseType:            "dir-24-8-bench-compare-lib",
		Description:             map[string]string{"en": "record id of the longest matching corpus prefix"},
		IPVersion:               family,
		RecordSize:              *recordSize,
		IncludeReservedNetworks: true,
		DisableIPv4Aliasing:     true,
		BuildEpoch:              1,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, "mmdbwriter:", err)
		os.Exit(1)
	}
	order := make([]int, len(ps))
	for i := range order {
		order[i] = i
	}
	sort.SliceStable(order, func(a, b int) bool { return ps[order[a]].Bits < ps[order[b]].Bits })
	bits := 32
	w := 4
	if family == 6 {
		bits, w = 128, 16
	}
	for _, i := range order {
		p := &ps[i]
		ip := make(net.IP, w)
		copy(ip, p.Addr[:w])
		if err := tree.Insert(&net.IPNet{IP: ip, Mask: net.CIDRMask(p.Bits, bits)}, mmdbtype.Uint32(p.ID)); err != nil {
			fmt.Fprintf(os.Stderr, "insert %v/%d: %v\n", ip, p.Bits, err)
			os.Exit(1)
		}
	}
	insertNs := time.Since(t0).Nanoseconds()
	tmp := *outPath + ".tmp"
	f, err := os.Create(tmp)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	t1 := time.Now()
	n, err := tree.WriteTo(f)
	if err == nil {
		err = f.Close()
	}
	if err == nil {
		err = os.Rename(tmp, *outPath)
	}
	if err != nil {
		os.Remove(tmp)
		fmt.Fprintln(os.Stderr, "write:", err)
		os.Exit(1)
	}
	writer := "github.com/maxmind/mmdbwriter"
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, d := range bi.Deps {
			if d.Path == writer {
				writer += "@" + d.Version
			}
		}
	}
	json.NewEncoder(os.Stdout).Encode(map[string]any{
		"type": "mmdb", "prefixes": len(ps), "family": family, "record_size": *recordSize,
		"insert_ns": insertNs, "write_ns": time.Since(t1).Nanoseconds(), "file_bytes": n, "writer": writer,
	})
}
