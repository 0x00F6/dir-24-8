//! Concurrent readers share `&IpTable` without synchronization.

mod common;

use std::net::IpAddr;

use common::Gen;
use dir_24_8::IpTable;
use dir_24_8::Replace;

#[test]
fn concurrent_readers_see_consistent_results() {
    fn assert_sync<T: Sync + Send>() {}
    assert_sync::<IpTable<String>>();

    let mut g = Gen::new(11);
    let mut t: IpTable<String> = IpTable::new();
    for i in 0..2000 {
        let n = g.net();
        t.insert(n, (i % 3 != 0).then(|| format!("route-{i}")), Replace)
            .unwrap();
    }
    let ips: Vec<IpAddr> = (0..4096).map(|_| g.ip()).collect();
    let expected: Vec<Option<(dir_24_8::IpNet, Option<String>)>> = ips
        .iter()
        .map(|&ip| t.lookup(ip).map(|m| (m.prefix, m.data.cloned())))
        .collect();
    let table = &t;
    std::thread::scope(|s| {
        for th in 0..8 {
            let ips = &ips;
            let expected = &expected;
            s.spawn(move || {
                let mut out = vec![None; ips.len()];
                for round in 0..20 {
                    if (round + th) % 2 == 0 {
                        table.lookup_batch_into(ips, &mut out).unwrap();
                    } else {
                        for (o, &ip) in out.iter_mut().zip(ips) {
                            *o = table.lookup(ip);
                        }
                    }
                    for (o, e) in out.iter().zip(expected) {
                        let got = o.map(|m| (m.prefix, m.data.cloned()));
                        assert_eq!(&got, e);
                    }
                }
            });
        }
    });
}
