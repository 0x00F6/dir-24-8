# Architecture

## Components

```mermaid
flowchart LR
    subgraph API["Public API (src/table, src/lookup)"]
        T["IpTable&lt;T&gt;<br/>insert(mode: Replace / Merge) / remove<br/>lookup / exist / batches"]
        C["Config<br/>(limits, huge pages)"]
        R["LookupMatch&lt;'a, T&gt;<br/>MatchId&lt;'a&gt;"]
    end
    subgraph Engines
        V4["ipv4::Ipv4Engine<br/>DIR-24-8"]
        V6["ipv6::Ipv6Engine<br/>24/8 multi-level trie"]
    end
    subgraph Shared["Shared state (src/route, src/table)"]
        S["RouteStore<br/>slots: prefix + Option&lt;T&gt;"]
        I["PrefixIndex<br/>HashMap + per-length counts"]
        G["GroupPool<br/>flat tbl8, free list"]
        B["TableBuffer<br/>mmap / huge pages"]
    end
    subgraph Kernels["Kernels (src/simd)"]
        K0["scalar<br/>(single lookups, default batches)"]
        K1["avx2 (batch-simd)"]
        K2["avx512 (batch-simd)"]
        K3["neon (batch-simd)"]
    end
    T --> C
    T --> V4
    T --> V6
    V4 --> S & I & G & B
    V6 --> S & I & G & B
    T -- "single: scalar; batches: Kernel (detected at construction)" --> Kernels
    Kernels -- "raw cells" --> T
    T --> R
```

* `IpTable<T>` owns at most one engine per family, created on the first
  insertion of that family. Each engine owns its root (`tbl24`), its group
  pool, its route store and its prefix index.
* Kernels never see payloads: they read `u32` cells through a `Tables` view
  and return raw cells. The facade (or a batch sink) converts a cell into a
  borrowed `LookupMatch` with one slot read.
* Single lookups and `exist` call the scalar kernels directly. Batches go
  through `Kernel`, detected once per table: only `Scalar` without the
  `batch-simd` feature; with it, AVX-512 then AVX2 (x86-64) or NEON
  (AArch64) when the CPU has them. Its SIMD variants can only be built
  after runtime feature detection succeeded, which is what makes the
  `unsafe` calls of `#[target_feature]` kernels sound.

## Cell encoding

| Cell value | Meaning |
|---|---|
| `0` | no route covers the cell |
| `1 ..= 0x7FFF_FFFF` | terminal: route slot `value - 1` |
| `0x8000_0000 \| g` (`g < 2^23`) | secondary group `g` |

The group flag is the sign bit: AVX2 masked gathers use it directly as their
mask, AVX-512 obtains a `__mmask16` with one signed compare, NEON uses
`vcltzq_s32`. `(cell << 8) | byte` drops the flag and yields `g * 256 +
byte < 2^31`, a non-negative gather index. All cells covered by one route
hold the same slot, hence share the same payload.

## IPv4: DIR-24-8

```mermaid
flowchart LR
    A["addr: u32 = from_be_bytes(octets)"] --> I24["idx = addr >> 8"]
    I24 --> T24[("tbl24[idx]<br/>2^24 cells, 64 MiB")]
    T24 -- "0" --> N["None"]
    T24 -- "route" --> S["slot → LookupMatch"]
    T24 -- "group g" --> I8["(g << 8) | (addr & 0xFF)"]
    I8 --> T8[("tbl8 group g<br/>256 cells")]
    T8 -- "0" --> N
    T8 -- "route" --> S
```

At most **two dependent table accesses** whatever the prefix length; one
more read of the route slot to build the borrowed result (`exist` skips it).

## IPv6: 24/8 trie (FIB6 layout)

```mermaid
flowchart LR
    A["octets[0..16]"] --> R["idx = o0&lt;&lt;16 | o1&lt;&lt;8 | o2"]
    R --> T24[("root[idx]")]
    T24 -- "group" --> L1[("level 1 group<br/>indexed by o3")]
    L1 -- "group" --> L2[("level 2 group<br/>o4")]
    L2 -- "..." --> L13[("level 13 group<br/>o15")]
    T24 -- "route / 0" --> E["terminal"]
    L1 -- "route / 0" --> E
    L2 -- "route / 0" --> E
    L13 -- "route / 0" --> E
```

A route of length `L > 24` ends in level `k = ceil((L - 24) / 8)` and covers
`2^(24 + 8k - L)` cells of its group, so lengths that are not multiples of 8
are expanded inside a single group. A lookup costs **1 to 14 dependent
accesses** (a /48 route: 4, a /64: 6), unlike the IPv4 bound of two.

## Maintenance

Invariant: every cell holds the longest route covering all of its addresses.

* **Insert** `P/L`: inside the range of `P`, overwrite exactly the cells
  whose current winner is shorter than `L` (or empty), descending into
  nested groups. A missing group on the path is created and filled with the
  cell it replaces (inheritance of the best less specific route).
* **Remove** `P/L`: find the parent (longest remaining route strictly
  containing `P`, through the prefix index and its per-length counts), then
  rewrite every cell equal to `P`'s cell with the parent's cell.
* **Fold**: a group whose 256 cells become identical terminal cells is
  folded into its parent cell and pushed on the free list (bottom-up along
  the IPv6 path). Released groups are reused before the buffer grows.
* **Atomicity**: limits are checked first, then every fallible allocation
  (slot, index entry, groups) is reserved, and only then are cells written.
  A capacity or allocation error leaves the table unchanged.

## Batches

```mermaid
sequenceDiagram
    participant U as caller (&[K], &mut [Option<LookupMatch>])
    participant D as batch driver (stack buffers)
    participant K as kernel (scalar / AVX2 / AVX-512 / NEON)
    participant S as sink (LookupMatch / bool / MatchId)
    U->>D: keys (validated lengths)
    loop chunks of 64 (homogeneous) or 256 (mixed)
        D->>D: convert keys / partition by family (positions kept)
        D->>K: raw keys
        K-->>D: raw cells
        D->>S: prefetch slots of this chunk
        D->>S: materialize previous chunk at original positions
    end
```

Kernels:

* IPv4 x86: phase 1 gathers the roots of a 64-128 key block (independent
  gathers), phase 2 runs masked gathers only for vectors with extended lanes.
* IPv6 x86: four vectors (64 lanes AVX-512, 32 lanes AVX2) are transposed
  into 32-bit words and walk the levels in lock step; at step `k` every
  active lane consumes octet `2 + k`, so the per-lane position is implicit
  in the step counter; finished lanes are masked off and keep their
  terminal cell.
* NEON: vector index math (`vshrq_n_u32`, `vorrq_u32`, `vminq_u32`,
  `vcltzq_s32`, `vld4q_u32` transposes) with one scalar load per active lane.
* Scalar: roots of a whole block first, then branch-free secondary steps
  (inactive lanes read the reserved, L1-resident group 0).

## Snapshots (feature `snapshot`)

```mermaid
flowchart LR
    T["IpTable&lt;T&gt;<br/>route stores"] -- "columns: lengths, addresses,<br/>CBOR payloads" --> E["XZ encoder<br/>(liblzma, CRC64)"]
    E --> O["Vec&lt;u8&gt; / BufWriter&lt;File&gt; (temp + rename) / any Write"]
    S["&amp;[u8] / BufReader&lt;File&gt; / any BufRead"] --> D["XZ decoder<br/>(memory limit)"]
    D -- "validated prefixes, payloads" --> N["new IpTable&lt;T&gt;<br/>(inserts, kernels resolved here)"]
```

* The source of truth is the route store of each family: one slot per
  inserted CIDR, masked or not, with its `Option<T>`. The lookup tables
  only hold winners and are never saved; loading reinserts the routes, so
  `tbl24`, groups, the prefix index and the batch kernel detection are rebuilt
  for the loading CPU.
* Saving walks the stores (slot order, or sorted with `EntryOrder::Sorted`)
  three times per family: lengths and addresses block by block, then
  payloads. Loading inserts the prefixes with no payload, then assigns the
  payloads in slot order: a fresh store allocates slots in insertion order,
  so no prefix list is kept.
* No adaptation of the table was needed: the stores already enumerate the
  exact logical state, at no extra memory cost.

## Memory

| Item | Cost |
|---|---|
| `tbl24` per family | 64 MiB reserved on the first insertion (mmap, lazily zeroed: resident pages grow with the address ranges touched) |
| secondary group | 1 KiB, buffer grows geometrically (copy on growth) |
| route slot | `size_of::<(Ipv4Net or Ipv6Net, Option<T>, bool)>()`; 16 B for `T = u32` (IPv4), 28 B (IPv6) |
| prefix index | `HashMap<prefix, u32>` |

Typical footprints are in [performance.md](performance.md).
