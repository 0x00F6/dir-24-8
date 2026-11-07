//! Batch kernel selection: scalar by default, the CPU's SIMD kernel with
//! the `batch-simd` feature. The kernels' agreement with the scalar
//! reference is tested inside the crate (`lookup::dispatch`) for every
//! kernel the CPU supports, and through the oracle tests for this one.

use dir_24_8::{IpNet, IpTable, Replace};

/// The kernel `IpTable` must report on this build and CPU.
fn expected_kernel() -> &'static str {
    #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            return "avx512";
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return "avx2";
        }
    }
    #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return "neon";
        }
    }
    "scalar"
}

#[test]
fn batch_kernel_follows_the_feature_and_the_cpu() {
    let table: IpTable<u8> = IpTable::new();
    let kernel = table.stats().batch_kernel;
    eprintln!("batch kernel on this build and CPU: {kernel}");
    assert_eq!(kernel, expected_kernel());
    if cfg!(not(feature = "batch-simd")) {
        assert_eq!(kernel, "scalar", "no SIMD without the batch-simd feature");
    }
    assert!(
        table
            .stats()
            .to_string()
            .contains(&format!("batch kernel: {kernel}"))
    );
    assert!(format!("{table:?}").contains(kernel));
}

#[test]
fn batch_and_single_lookups_agree() {
    // Single lookups run the scalar kernels, batches the detected kernel:
    // both must return the same matches, including the payload reference.
    let mut table = IpTable::new();
    for (i, net) in [
        "0.0.0.0/0",
        "10.0.0.0/8",
        "10.1.2.0/24",
        "10.1.2.128/25",
        "10.1.2.200/32",
        "::/0",
        "2001:db8::/32",
        "2001:db8:1::/48",
        "2001:db8:1:2:3:4:5:6/128",
    ]
    .iter()
    .enumerate()
    {
        table
            .insert(net.parse::<IpNet>().unwrap(), Some(i), Replace)
            .unwrap();
    }
    let keys: Vec<std::net::IpAddr> = [
        "10.1.2.200",
        "10.1.2.201",
        "10.1.2.1",
        "10.9.9.9",
        "192.0.2.1",
        "2001:db8:1:2:3:4:5:6",
        "2001:db8:1:2:3:4:5:7",
        "2001:db8:2::1",
        "2002::1",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .cycle()
    .take(1_000)
    .collect();
    let mut out = vec![None; keys.len()];
    table.lookup_batch_into(&keys, &mut out).unwrap();
    let mut exist = vec![false; keys.len()];
    table.exist_batch_into(&keys, &mut exist).unwrap();
    for (i, &ip) in keys.iter().enumerate() {
        let single = table.lookup(ip);
        assert_eq!(out[i].map(|m| m.prefix), single.map(|m| m.prefix), "{ip}");
        assert!(core::ptr::eq(
            out[i].and_then(|m| m.data).unwrap(),
            single.and_then(|m| m.data).unwrap()
        ));
        assert_eq!(exist[i], table.exist(ip), "{ip}");
    }
    let array = table.lookup_array(&[keys[0], keys[4]]);
    assert_eq!(array[0].and_then(|m| m.data), Some(&4));
    assert_eq!(array[1].and_then(|m| m.data), Some(&0));
    assert_eq!(table.exist_array(&[keys[0], keys[8]]), [true, true]);
}
