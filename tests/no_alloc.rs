//! The lookup path never allocates once the table is built.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use common::Gen;
use dir_24_8::Replace;
use dir_24_8::{IpTable, MatchId};

struct Counting;

thread_local! {
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator, only adds a thread-local count.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn allocations<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = COUNT.with(Cell::get);
    let r = f();
    (COUNT.with(Cell::get) - before, r)
}

#[test]
fn lookups_do_not_allocate() {
    let mut g = Gen::new(21);
    let mut t: IpTable<String> = IpTable::new();
    for i in 0..3000 {
        let n = g.net();
        t.insert(n, Some(format!("{i}")), Replace).unwrap();
    }
    let ips: Vec<IpAddr> = (0..1000).map(|_| g.ip()).collect();
    let v4: Vec<Ipv4Addr> = (0..1000).map(|_| Ipv4Addr::from(g.addr4())).collect();
    let v6: Vec<Ipv6Addr> = (0..1000).map(|_| Ipv6Addr::from(g.addr6())).collect();
    let n4: Vec<u32> = v4.iter().map(|&a| a.into()).collect();
    let n6: Vec<u128> = v6.iter().map(|&a| a.into()).collect();
    let mut out = vec![None; 1000];
    let mut ex = vec![false; 1000];
    let mut ids = vec![MatchId::NONE; 1000];
    let (count, hits) = allocations(|| {
        let mut hits = 0usize;
        for &ip in &ips {
            hits += usize::from(t.lookup(ip).is_some());
            hits += usize::from(t.exist(ip));
            hits += usize::from(t.lookup_data(ip).is_some());
        }
        t.lookup_batch_into(&ips, &mut out).unwrap();
        t.lookup_batch_into(&v4, &mut out).unwrap();
        t.lookup_batch_into(&v6, &mut out).unwrap();
        t.lookup_batch_into(&n4, &mut out).unwrap();
        t.lookup_batch_into(&n6, &mut out).unwrap();
        t.exist_batch_into(&ips, &mut ex).unwrap();
        t.lookup_ids_into(&ips, &mut ids).unwrap();
        t.lookup_ids_into(&n4, &mut ids).unwrap();
        hits += ids.iter().filter(|&&id| t.resolve(id).is_some()).count();
        let arr = t.lookup_array(&[ips[0], ips[1], ips[2], ips[3]]);
        hits += arr.iter().flatten().count();
        hits += t
            .exist_array(&[n4[0], n4[1]])
            .iter()
            .filter(|&&x| x)
            .count();
        hits
    });
    assert_eq!(count, 0, "lookup path allocated");
    assert!(hits > 0);
}
