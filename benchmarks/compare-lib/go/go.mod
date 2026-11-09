// Native Go runner (and MMDB writer) of the libraries part of `make bench-compare`. One
// module, one toolchain, one set of build flags for every Go library.
module dir248bench/compare-lib

go 1.24.0

toolchain go1.27.3

require (
	github.com/gaissmai/bart v0.30.0
	github.com/libp2p/go-cidranger v1.1.0
	github.com/maxmind/mmdbwriter v1.2.0
	github.com/phemmer/go-iptrie v0.0.0-20240326174613-ba542f5282c9
	github.com/yl2chen/cidranger v1.0.2
)

require (
	github.com/oschwald/maxminddb-golang/v2 v2.1.1 // indirect
	go4.org/netipx v0.0.0-20231129151722-fdeea329fbba // indirect
	golang.org/x/sys v0.38.0 // indirect
)
