//! `bench-compare-lib` binary (orchestrator and Rust runner), see
//! `dir_24_8_compare::libcmp`.

#[global_allocator]
static ALLOC: dir_24_8_compare::measure::Counting = dir_24_8_compare::measure::Counting;

fn main() {
    dir_24_8_compare::libcmp::main();
}
