//! The compared libraries. The order fixes the report colors.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {
    Rust,
    Go,
    C,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct LibDef {
    pub id: &'static str,
    pub lang: &'static str,
    pub repo: &'static str,
    pub runner: RunnerKind,
    /// Package, module or crate that is linked.
    pub package: &'static str,
    /// `native`, `adapted` or `unsupported`.
    pub contains: &'static str,
    pub lookup: &'static str,
    pub contains_api: &'static str,
    pub lookup_api: &'static str,
    pub reference: bool,
    /// Same library with another configuration (shares its color, dashed).
    pub variant_of: Option<&'static str>,
    pub notes: &'static str,
}

impl LibDef {
    pub fn supports_lookup(&self) -> bool {
        self.lookup != "unsupported"
    }
}

pub const LIBS: [LibDef; 8] = [
    LibDef {
        id: "dir-24-8",
        lang: "Rust",
        repo: "this repository",
        runner: RunnerKind::Rust,
        package: "dir-24-8",
        contains: "native",
        lookup: "native",
        contains_api: "IpTable::exist_ipv4 / exist_ipv6",
        lookup_api: "IpTable::lookup_ipv4 / lookup_ipv6 (payload)",
        reference: true,
        variant_of: None,
        notes: "Reference implementation (this project): IPv4 DIR-24-8, IPv6 24/8 multi-level trie, default Config. Single lookups and exist always run the scalar kernels (the batch-simd feature only changes batch lookups, which this comparison does not use).",
    },
    LibDef {
        id: "go-cidranger-libp2p",
        lang: "Go",
        repo: "https://github.com/libp2p/go-cidranger",
        runner: RunnerKind::Go,
        package: "github.com/libp2p/go-cidranger",
        contains: "native",
        lookup: "adapted",
        contains_api: "Ranger.Contains(net.IP)",
        lookup_api: "LPM adapted: Ranger.ContainingNetworks(net.IP), then the entry with the longest prefix",
        reference: false,
        variant_of: None,
        notes: "Fork of yl2chen/cidranger by libp2p (\"due to upstream inactivity\"), same path-compressed trie (NewPCTrieRanger) with later changes. Not an independent algorithm.",
    },
    LibDef {
        id: "cidranger-yl2chen",
        lang: "Go",
        repo: "https://github.com/yl2chen/cidranger",
        runner: RunnerKind::Go,
        package: "github.com/yl2chen/cidranger",
        contains: "native",
        lookup: "adapted",
        contains_api: "Ranger.Contains(net.IP)",
        lookup_api: "LPM adapted: Ranger.ContainingNetworks(net.IP), then the entry with the longest prefix",
        reference: false,
        variant_of: None,
        notes: "Original cidranger (NewPCTrieRanger, path-compressed trie); go-cidranger-libp2p is its fork. Same adapter logic as the fork, distinct import.",
    },
    LibDef {
        id: "go-iptrie",
        lang: "Go",
        repo: "https://github.com/phemmer/go-iptrie",
        runner: RunnerKind::Go,
        package: "github.com/phemmer/go-iptrie",
        contains: "native",
        lookup: "native",
        contains_api: "Trie.Contains(netip.Addr)",
        lookup_api: "Trie.Find(netip.Addr) (value of the most specific network)",
        reference: false,
        variant_of: None,
        notes: "Path-compressed binary trie; IPv4 stored as IPv4-mapped IPv6 internally; values stored as `any`.",
    },
    LibDef {
        id: "bart",
        lang: "Go",
        repo: "https://github.com/gaissmai/bart",
        runner: RunnerKind::Go,
        package: "github.com/gaissmai/bart",
        contains: "native",
        lookup: "native",
        contains_api: "Table[uint32].Contains(netip.Addr)",
        lookup_api: "Table[uint32].Lookup(netip.Addr)",
        reference: false,
        variant_of: None,
        notes: "Balanced Routing Table: multibit trie, stride 8, popcount-compressed sparse arrays (Table, with values).",
    },
    LibDef {
        id: "prefix-trie",
        lang: "Rust",
        repo: "https://github.com/tiborschneider/prefix-trie",
        runner: RunnerKind::Rust,
        package: "prefix-trie",
        contains: "adapted",
        lookup: "native",
        contains_api: "membership adapted: PrefixMap::get_lpm(host prefix).is_some() (no address membership API)",
        lookup_api: "PrefixMap::get_lpm(&host /32 or /128 prefix)",
        reference: false,
        variant_of: None,
        notes: "Binary prefix trie keyed by ipnet prefixes; queries are host prefixes built before timing.",
    },
    LibDef {
        id: "iprange",
        lang: "Rust",
        repo: "https://github.com/sticnarf/iprange-rs",
        runner: RunnerKind::Rust,
        package: "iprange",
        contains: "native",
        lookup: "unsupported",
        contains_api: "IpRange::contains(&addr)",
        lookup_api: "N/A - set without associated values",
        reference: false,
        variant_of: None,
        notes: "Set of networks merged by simplify(): stores fewer ranges than input prefixes; membership only.",
    },
    LibDef {
        id: "libmaxminddb",
        lang: "C",
        repo: "https://github.com/maxmind/libmaxminddb",
        runner: RunnerKind::C,
        package: "libmaxminddb",
        contains: "native",
        lookup: "native",
        contains_api: "MMDB_lookup_sockaddr + found_entry",
        lookup_api: "MMDB_lookup_sockaddr + MMDB_aget_value (uint32 payload = record id)",
        reference: false,
        variant_of: None,
        notes: "Official C reader of the MaxMind DB format; reads an MMDB file written from the same corpus by MaxMind's Go writer (mmdbwriter, preparation only), memory-mapped (MMDB_MODE_MMAP).",
    },
];

pub fn by_id(id: &str) -> Option<&'static LibDef> {
    LIBS.iter().find(|l| l.id == id)
}
