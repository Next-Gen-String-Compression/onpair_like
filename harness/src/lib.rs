//! LIKE-benchmark harness library. The trust root (oracle, gate, clock,
//! formats) lives here; candidates and scanners plug in via the C-ABI
//! contract in `contract/lb_candidate.h` (mirrored by the `lb-abi` crate).

pub mod bitmap;
pub mod chunks;
pub mod cpu;
pub mod dataset;
pub mod gen;
pub mod oracle;
pub mod registry;
pub mod results;
pub mod runner;
pub mod spec;
pub mod suite;
pub mod timing;
