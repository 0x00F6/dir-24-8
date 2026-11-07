# dir-24-8-derive

`#[derive(DeepMerge)]` for [dir-24-8](https://crates.io/crates/dir-24-8).

Do not depend on this crate directly: enable the `derive` feature of
`dir-24-8`, which re-exports the macro next to the `DeepMerge` trait.

```toml
[dependencies]
dir-24-8 = { version = "0.1", features = ["derive"] }
```

```rust
use dir_24_8::DeepMerge;

#[derive(DeepMerge)]
struct Info {
    tags: Vec<String>,           // concatenated
    asn: u32,                    // incoming value wins
    #[deep_merge(skip)]
    first_seen: u64,             // existing value kept
    #[deep_merge(replace)]
    raw: Vec<u8>,                // replaced instead of concatenated
}
```

Structs merge field by field; enums merge field by field when both values
are the same variant and are replaced otherwise. License: MIT OR Apache-2.0.
