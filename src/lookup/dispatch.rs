//! Batch kernel selection and static dispatch.
//!
//! Single lookups always call the scalar kernels directly. Batch lookups go
//! through [`Kernel`]: without the `batch-simd` feature it only has the
//! scalar variant; with it, [`Kernel::detect`] picks the preferred SIMD
//! kernel the running CPU supports.

use crate::simd::scalar;
use crate::simd::tables::Tables;

#[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
use crate::simd::neon;
#[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
use crate::simd::{avx2, avx512};

/// Batch kernel family, detected once per table.
///
/// Invariant: a SIMD variant only exists after [`Kernel::detect`] confirmed
/// the required extensions on the running CPU, which is what makes the
/// `unsafe` calls of the `#[target_feature]` kernels below sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kernel {
    Scalar,
    #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
    Avx2,
    #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
    Avx512,
    #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
    Neon,
}

impl Kernel {
    /// Preferred batch kernel on the running CPU: with `batch-simd`,
    /// AVX-512 (F, BW, VL) then AVX2 on x86-64 (order measured, see
    /// `docs/performance.md`) and NEON on AArch64; scalar otherwise.
    pub(crate) fn detect() -> Self {
        #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
        {
            if std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("avx512bw")
                && std::arch::is_x86_feature_detected!("avx512vl")
            {
                return Kernel::Avx512;
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                return Kernel::Avx2;
            }
        }
        #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
        {
            if std::arch::is_aarch64_feature_detected!("neon") {
                return Kernel::Neon;
            }
        }
        Kernel::Scalar
    }

    /// Stable lowercase name (`"scalar"`, `"avx2"`, `"avx512"`, `"neon"`).
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Kernel::Scalar => "scalar",
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx2 => "avx2",
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx512 => "avx512",
            #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
            Kernel::Neon => "neon",
        }
    }

    /// `keys.len() == out.len()` is required.
    #[inline(always)]
    pub(crate) fn v4_batch(self, t: &Tables<'_>, keys: &[u32], out: &mut [u32]) {
        debug_assert_eq!(keys.len(), out.len());
        match self {
            Kernel::Scalar => scalar::v4_batch(t, keys, out),
            // SAFETY (all arms below): the variant exists only if the CPU
            // supports the kernel's target features (see type invariant).
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx2 => unsafe { avx2::v4_batch(t, keys, out) },
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx512 => unsafe { avx512::v4_batch(t, keys, out) },
            #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
            Kernel::Neon => unsafe { neon::v4_batch(t, keys, out) },
        }
    }

    /// `keys.len() == out.len()` is required.
    #[inline(always)]
    pub(crate) fn v6_batch(self, t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32]) {
        debug_assert_eq!(keys.len(), out.len());
        match self {
            Kernel::Scalar => scalar::v6_batch(t, keys, out),
            // SAFETY: see `v4_batch`.
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx2 => unsafe { avx2::v6_batch(t, keys, out) },
            #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
            Kernel::Avx512 => unsafe { avx512::v6_batch(t, keys, out) },
            #[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
            Kernel::Neon => unsafe { neon::v6_batch(t, keys, out) },
        }
    }
}

/// Every SIMD kernel the running CPU supports must return exactly the cells
/// of the scalar kernel. The integration tests only exercise the kernel
/// `detect` picks; these tests also cover the others (AVX2 on an AVX-512
/// machine), every tail length and unaligned key slices.
#[cfg(all(test, feature = "batch-simd"))]
mod tests {
    use core::net::{Ipv4Addr, Ipv6Addr};

    use ipnet::{IpNet, Ipv4Net, Ipv6Net};

    use super::Kernel;
    use crate::simd::scalar;
    use crate::{IpTable, Replace};

    /// Kernels the running CPU can execute, the detected one first.
    fn simd_kernels() -> Vec<Kernel> {
        let mut out = Vec::new();
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("avx512bw")
                && std::arch::is_x86_feature_detected!("avx512vl")
            {
                out.push(Kernel::Avx512);
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                out.push(Kernel::Avx2);
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            if std::arch::is_aarch64_feature_detected!("neon") {
                out.push(Kernel::Neon);
            }
        }
        if out.is_empty() {
            eprintln!("no SIMD batch kernel runs on this CPU: comparison not executed");
        } else {
            assert_eq!(
                out[0],
                Kernel::detect(),
                "detect picks the preferred kernel"
            );
        }
        out
    }

    /// SplitMix64.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn u128(&mut self) -> u128 {
            (u128::from(self.next()) << 64) | u128::from(self.next())
        }
    }

    /// Nested IPv4 and IPv6 routes around a few hot regions, with every
    /// prefix length represented, plus the highest root index of each
    /// family (clamp and bounds edge).
    fn table(seed: u64) -> (IpTable<u32>, Vec<u32>, Vec<[u8; 16]>) {
        let mut rng = Rng(seed);
        let hot4: Vec<u32> = (0..6).map(|_| rng.next() as u32).collect();
        let hot6: Vec<u128> = (0..6).map(|_| rng.u128()).collect();
        let mut t = IpTable::new();
        let mut nets: Vec<IpNet> = Vec::new();
        for _ in 0..3_000 {
            let len = (rng.below(33)) as u8;
            let base = hot4[rng.below(6) as usize]
                ^ (rng.next() as u32)
                    .checked_shr(rng.below(33) as u32)
                    .unwrap_or(0);
            nets.push(IpNet::V4(
                Ipv4Net::new(Ipv4Addr::from(base), len).unwrap().trunc(),
            ));
            let len = (rng.below(129)) as u8;
            let base = hot6[rng.below(6) as usize]
                ^ rng.u128().checked_shr(rng.below(129) as u32).unwrap_or(0);
            nets.push(IpNet::V6(
                Ipv6Net::new(Ipv6Addr::from(base), len).unwrap().trunc(),
            ));
        }
        nets.push("255.255.255.255/32".parse().unwrap());
        nets.push(
            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128"
                .parse()
                .unwrap(),
        );
        for (i, n) in nets.iter().enumerate() {
            t.insert(*n, Some(i as u32), Replace).unwrap();
        }
        // Keys: route boundaries and their neighbors, then random keys.
        let mut k4 = Vec::new();
        let mut k6 = Vec::new();
        for n in &nets {
            match n {
                IpNet::V4(n) => {
                    let (lo, hi) = (u32::from(n.network()), u32::from(n.broadcast()));
                    k4.extend([lo, hi, lo.wrapping_sub(1), hi.wrapping_add(1)]);
                }
                IpNet::V6(n) => {
                    let (lo, hi) = (u128::from(n.network()), u128::from(n.broadcast()));
                    for a in [lo, hi, lo.wrapping_sub(1), hi.wrapping_add(1)] {
                        k6.push(a.to_be_bytes());
                    }
                }
            }
        }
        k4.extend((0..4_000).map(|_| rng.next() as u32));
        k6.extend((0..4_000).map(|_| rng.u128().to_be_bytes()));
        (t, k4, k6)
    }

    /// Batch lengths covering empty input, every tail below two 64-lane
    /// blocks, block boundaries and a long run.
    fn lengths(total: usize) -> impl Iterator<Item = usize> {
        (0..=130).chain([255, 256, 257, 1_000, total])
    }

    #[test]
    fn simd_batch_kernels_match_scalar() {
        let kernels = simd_kernels();
        for seed in [1, 2] {
            let (table, k4, k6) = table(seed);
            let e4 = table.ipv4_engine().expect("IPv4 routes inserted");
            let e6 = table.ipv6_engine().expect("IPv6 routes inserted");
            let (t4, t6) = (e4.tables(), e6.tables());

            let mut want4 = vec![0u32; k4.len()];
            scalar::v4_batch(&t4, &k4, &mut want4);
            for (i, &k) in k4.iter().enumerate() {
                assert_eq!(
                    want4[i],
                    scalar::v4_one(&t4, k),
                    "scalar v4 batch/single {k:#x}"
                );
            }
            let mut want6 = vec![0u32; k6.len()];
            scalar::v6_batch(&t6, &k6, &mut want6);
            for (i, k) in k6.iter().enumerate() {
                let one = scalar::v6_one(&t6, u128::from_be_bytes(*k));
                assert_eq!(want6[i], one, "scalar v6 batch/single {k:x?}");
            }

            for &kernel in &kernels {
                let name = kernel.name();
                for offset in 0..4 {
                    let (k4, w4) = (&k4[offset..], &want4[offset..]);
                    let (k6, w6) = (&k6[offset..], &want6[offset..]);
                    for n in lengths(k4.len()) {
                        let mut got = vec![u32::MAX; n];
                        kernel.v4_batch(&t4, &k4[..n], &mut got);
                        assert_eq!(
                            got,
                            w4[..n],
                            "{name} v4 seed {seed} offset {offset} len {n}"
                        );
                    }
                    for n in lengths(k6.len()) {
                        let mut got = vec![u32::MAX; n];
                        kernel.v6_batch(&t6, &k6[..n], &mut got);
                        assert_eq!(
                            got,
                            w6[..n],
                            "{name} v6 seed {seed} offset {offset} len {n}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn simd_batch_kernels_on_a_root_only_table() {
        // One short route per family: no secondary group besides the
        // reserved group 0, every cell is terminal or empty.
        let mut table = IpTable::new();
        table
            .insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some(1u32), Replace)
            .unwrap();
        table
            .insert("2001:db8::/32".parse::<IpNet>().unwrap(), Some(2), Replace)
            .unwrap();
        let t4 = table.ipv4_engine().unwrap().tables();
        let t6 = table.ipv6_engine().unwrap().tables();
        let mut rng = Rng(7);
        let k4: Vec<u32> = (0..300)
            .map(|i| {
                if i % 2 == 0 {
                    0x0A00_0000 | (rng.next() as u32 >> 8)
                } else {
                    rng.next() as u32
                }
            })
            .collect();
        let k6: Vec<[u8; 16]> = (0..300)
            .map(|i| {
                let a = rng.u128();
                let a = if i % 2 == 0 {
                    (0x2001_0db8u128 << 96) | (a >> 32)
                } else {
                    a
                };
                a.to_be_bytes()
            })
            .collect();
        let mut want4 = vec![0; k4.len()];
        scalar::v4_batch(&t4, &k4, &mut want4);
        let mut want6 = vec![0; k6.len()];
        scalar::v6_batch(&t6, &k6, &mut want6);
        assert!(want4.iter().any(|&c| c != 0) && want4.contains(&0));
        assert!(want6.iter().any(|&c| c != 0) && want6.contains(&0));
        for kernel in simd_kernels() {
            let mut got = vec![u32::MAX; k4.len()];
            kernel.v4_batch(&t4, &k4, &mut got);
            assert_eq!(got, want4, "{} v4", kernel.name());
            let mut got = vec![u32::MAX; k6.len()];
            kernel.v6_batch(&t6, &k6, &mut got);
            assert_eq!(got, want6, "{} v6", kernel.name());
        }
    }
}
