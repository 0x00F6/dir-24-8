# dir-24-8

**Fast IPv4 and IPv6 CIDR lookups in Rust**, zero-copy, with optional
AVX-512 / AVX2 / NEON batch lookups.

- **IPv4**: a true DIR-24-8, one or two memory reads per lookup.
- **IPv6**: a 24/8 multi-level trie, one read per level (at most 14).
- **Zero-copy**: lookups borrow the stored payload; no allocation, no clone, no
  lock, no `T: Clone` bound.
- **SIMD batches**: AVX-512, AVX2 or NEON, detected at runtime (feature
  `batch-simd`).
- **Safe updates**: insert, merge and remove in place; a failed mutation changes
  nothing.

⚠️ Memory: each family maps a 64 MiB root on its first insertion; expect about
160 B per IPv4 prefix and 1.7 KiB per IPv6 prefix at 1M prefixes.

## Installation

```toml
[dependencies]
dir-24-8 = "0.1"
```

- The crate is imported as `dir_24_8`.
- The default build uses scalar kernels and depends only on `ipnet` (and `libc`
  on Linux).

## Cargo features

| Feature | Default | Enables |
|---|:---:|---|
| `batch-simd` | no | AVX-512 / AVX2 (x86-64) and NEON (AArch64) batch kernels, selected at runtime; single lookups stay scalar |
| `derive` | no | `#[derive(DeepMerge)]` |
| `serde` | no | the `SerdeMerge` insert mode and `DeepMerge for serde_json::Value` |
| `snapshot` | no | compressed snapshots (XZ/LZMA2 through liblzma, payloads as CBOR) |

Example: `dir-24-8 = { version = "0.1", features = ["batch-simd", "snapshot"] }`.

## Usage

### Longest-prefix lookups

```rust
use std::net::IpAddr;
use dir_24_8::{IpNet, IpTable, Replace};

let mut table: IpTable<&str> = IpTable::new();
table.insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some("corp"), Replace).unwrap();
table.insert("10.1.0.0/16".parse::<IpNet>().unwrap(), None, Replace).unwrap(); // no payload
table.insert("2001:db8::/32".parse::<IpNet>().unwrap(), Some("doc"), Replace).unwrap();

let ip: IpAddr = "10.2.3.4".parse().unwrap();
let m = table.lookup(ip).unwrap();
assert_eq!(m.prefix.to_string(), "10.0.0.0/8");
assert_eq!(m.data, Some(&"corp")); // borrowed, never cloned

let m = table.lookup("10.1.9.9".parse().unwrap()).unwrap();
assert_eq!((m.prefix.to_string().as_str(), m.data), ("10.1.0.0/16", None));

assert_eq!(table.lookup_data("2001:db8::1".parse().unwrap()), Some(&"doc"));
assert!(table.exist("10.1.9.9".parse().unwrap()));
assert!(!table.exist("192.0.2.1".parse().unwrap()));
```

- `None`: no route covers the address.
- `Some(LookupMatch { data: None, .. })`: the winning route has no payload.
- A more specific route always wins, with or without payload.
- IPv4 and IPv6 are separate key spaces: `192.0.2.1` and `::ffff:192.0.2.1` differ.

### Batch lookups

- Keys: `&[IpAddr]` (mixed), `&[Ipv4Addr]`, `&[Ipv6Addr]`, `u32`, `u128`,
  `[u8; 4]` or `[u8; 16]`.
- Results go into caller buffers, in input order, without allocating.
- With `batch-simd`, batches run the CPU's SIMD kernel; every kernel returns the
  same results.

```rust
use std::net::{IpAddr, Ipv4Addr};
use dir_24_8::{IpNet, IpTable, MatchId, Replace};

let mut table: IpTable<u32> = IpTable::new();
table.insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some(8), Replace).unwrap();
table.insert("2001:db8::/32".parse::<IpNet>().unwrap(), Some(32), Replace).unwrap();

let keys: Vec<IpAddr> = vec![
    "10.1.2.3".parse().unwrap(),
    "192.0.2.1".parse().unwrap(),
    "2001:db8::1".parse().unwrap(),
];
let mut out = vec![None; keys.len()];
table.lookup_batch_into(&keys, &mut out).unwrap();
let data: Vec<Option<u32>> = out.iter().map(|m| m.and_then(|m| m.data).copied()).collect();
assert_eq!(data, [Some(8), None, Some(32)]);

// Fixed-size arrays are returned by value.
let [a, b] = table.lookup_array(&[0x0A00_0001u32, 0xC000_0201]);
assert_eq!((a.and_then(|m| m.data), b.is_none()), (Some(&8), true));
let covered = table.exist_array(&[Ipv4Addr::new(10, 9, 9, 9), Ipv4Addr::new(192, 0, 2, 1)]);
assert_eq!(covered, [true, false]);

// Compact 4-byte identifiers, resolved on demand.
let mut ids = [MatchId::NONE; 2];
table.lookup_ids_into(&[0x0A00_0001u32, 0xC000_0201], &mut ids).unwrap();
assert_eq!(table.resolve(ids[0]).unwrap().data, Some(&8));

// "scalar", or with `batch-simd` the CPU's kernel ("avx512", "avx2", "neon").
println!("batch kernel: {}", table.stats().batch_kernel);
```

### Insert modes and deep merge

| Mode | When the CIDR is present |
|---|---|
| `Replace` | its payload is replaced |
| `Merge` | deep merged (`T: DeepMerge`) |
| `MergeWith(f)` | merged by your closure |
| `SerdeMerge` | merged through serde (feature `serde`) |

```rust
use dir_24_8::{DeepMerge, IpNet, IpTable, Merge};

#[derive(DeepMerge)] // feature `derive`
struct Info {
    tags: Vec<String>, // concatenated
    asn: u32,          // incoming value wins
    #[deep_merge(skip)]
    first_seen: u64,   // existing value kept
}

let net: IpNet = "192.0.2.0/24".parse().unwrap();
let mut table = IpTable::new();
table.insert(net, Some(Info { tags: vec!["a".into()], asn: 1, first_seen: 10 }), Merge).unwrap();
table.insert(net, Some(Info { tags: vec!["b".into()], asn: 2, first_seen: 20 }), Merge).unwrap();
let info = table.lookup_data("192.0.2.1".parse().unwrap()).unwrap();
assert_eq!((info.tags.len(), info.asn, info.first_seen), (2, 2, 10));
```

### Updates and removal

- Removing a route restores the best remaining less specific route.
- A failed mutation (capacity, allocation) leaves the table unchanged.

```rust
use dir_24_8::{IpNet, IpTable, Replace};

let wide: IpNet = "10.0.0.0/8".parse().unwrap();
let narrow: IpNet = "10.1.0.0/16".parse().unwrap();
let mut table = IpTable::new();
table.insert(wide, Some("wide"), Replace).unwrap();
table.insert(narrow, Some("narrow"), Replace).unwrap();
assert_eq!(table.replace(narrow, Some("lab")), Some(Some("narrow"))); // never inserts
assert_eq!(table.remove(narrow), Some(Some("lab")));
assert_eq!(table.lookup_data("10.1.2.3".parse().unwrap()), Some(&"wide"));
```

### Compressed snapshots

- Feature `snapshot`: save to memory or to a file, streamed, with atomic file
  replacement.
- Loading validates the input and enforces configurable limits.

```rust
use dir_24_8::{IpNet, IpTable, LoadOptions, Replace};

let mut table = IpTable::new();
table.insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some(String::from("corp")), Replace).unwrap();
table.insert("10.1.0.0/16".parse::<IpNet>().unwrap(), None, Replace).unwrap();

let bytes: Vec<u8> = table.snapshot().unwrap();
let restored: IpTable<String> = IpTable::load(&bytes).unwrap();
assert_eq!(restored.get("10.1.0.0/16".parse::<IpNet>().unwrap()), Some(None));

let path = std::env::temp_dir().join(format!("readme-crates-{}.snap", std::process::id()));
table.snapshot_to_file(&path).unwrap();
let limits = LoadOptions::new().max_entries(1_000_000).max_value_bytes(4096);
let restored: IpTable<String> = IpTable::load_from_file_with_options(&path, &limits).unwrap();
assert_eq!(restored.len(), 2);
std::fs::remove_file(&path).unwrap();
```

## Examples

Runnable programs in the package's `examples` directory, each asserting its
results:

| Example | Shows | Features |
|---|---|---|
| `ipv4_lpm`, `ipv6_lpm` | longest-prefix match | |
| `batch_lookup`, `batch_kernel` | batch lookups, kernel of this CPU | `batch-simd` for the SIMD kernel |
| `exist_without_payload` | CIDR set without payload | |
| `custom_payload` | payload without `Clone` | |
| `typed_deep_merge` | typed deep merge | `derive` |
| `serde_merge` | serde deep merge | `serde` |
| `update_remove` | updates and removal | |
| `snapshot` | compressed snapshots | `snapshot` |
| `lookup_profile` | profiling workload | |

Run one from a checkout with `cargo run --example ipv4_lpm`.

## Documentation

- API documentation: [docs.rs/dir-24-8](https://docs.rs/dir-24-8).
- Project page and benchmark reports: [0x00f6.github.io/dir-24-8](https://0x00f6.github.io/dir-24-8/).

## Minimum supported Rust version

- Rust **1.89** or later, stable only (edition 2024).

## License

Licensed under either of

- Apache License, Version 2.0 (<https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license (<https://opensource.org/licenses/MIT>)

at your option.
