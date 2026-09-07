pub mod artifacts;
pub mod bitslice;
#[cfg(not(target_arch = "x86_64"))]
mod bs_des;
#[cfg(not(target_arch = "x86_64"))]
mod bs_sboxes;
pub mod cli;
pub mod compute;
pub mod config;
pub mod cpu;
pub mod cpu_verify;
pub mod formats;
pub mod gpu;
pub mod input;
pub mod local_lookup;
pub mod params;
mod platform;
pub mod remote_lookup;

pub const CHAIN_LEN: u32 = 881_689;
pub const TABLE_INDEX: u32 = 0;
pub const FIXED_CHALLENGE_HEX: &str = "1122334455667788";

pub use cli::{error_exit_code, run};
