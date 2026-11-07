//! Which kernel runs the batch lookups on this build and CPU.
//!
//! Single lookups always use the scalar kernels. Batches use them too,
//! unless the crate is built with the `batch-simd` feature: they then use
//! AVX-512 or AVX2 (x86-64) or NEON (AArch64) when the CPU has it.
//!
//! ```text
//! cargo run --release --example batch_kernel                        # scalar
//! cargo run --release --example batch_kernel --features batch-simd  # CPU's SIMD kernel
//! ```

use std::net::Ipv4Addr;

use dir_24_8::{Error, IpNet, IpTable, Replace};

fn main() -> Result<(), Error> {
    let mut table: IpTable<u32> = IpTable::new();
    table.insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some(8), Replace)?;
    table.insert("10.1.2.0/24".parse::<IpNet>().unwrap(), Some(24), Replace)?;
    table.insert("10.1.2.128/25".parse::<IpNet>().unwrap(), Some(25), Replace)?;

    let kernel = table.stats().batch_kernel;
    println!(
        "batch-simd feature: {}",
        if cfg!(feature = "batch-simd") {
            "on"
        } else {
            "off"
        }
    );
    println!("batch kernel: {kernel}, single lookups: scalar");
    if cfg!(not(feature = "batch-simd")) {
        assert_eq!(kernel, "scalar");
    }

    // Batches and single lookups return the same matches.
    let keys: Vec<Ipv4Addr> = (0..=255).map(|b| Ipv4Addr::new(10, 1, 2, b)).collect();
    let mut out = vec![None; keys.len()];
    table.lookup_batch_into(&keys, &mut out)?;
    for (k, m) in keys.iter().zip(&out) {
        assert_eq!(
            m.and_then(|m| m.data),
            table.lookup_ipv4(*k).and_then(|m| m.data)
        );
    }
    let wide = out
        .iter()
        .filter(|m| m.and_then(|m| m.data) == Some(&25))
        .count();
    assert_eq!(wide, 128);
    println!("{} batch results match the single lookups", out.len());
    Ok(())
}
