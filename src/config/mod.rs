//! Build configuration, version, and fundamental constants.

/// Library version string (kept in step with Cargo.toml).
pub const VERSION: &str = "0.1.0";
/// Largest alignment any allocator accepts.
pub const MAX_ALIGNMENT: usize = 4096;
/// Alignment that satisfies every fundamental type on the supported targets.
pub const DEFAULT_ALIGNMENT: usize = 16;
/// Largest block size: keeps pointer arithmetic on blocks well defined.
pub const MAX_BLOCK_SIZE: usize = isize::MAX as usize;
/// Debug instrumentation (poisoning, canaries, extra checks) follows the build profile.
pub const DEBUG_CHECKS: bool = cfg!(debug_assertions);
/// Fill for freshly allocated memory in debug builds.
pub const UNINIT_BYTE: u8 = 0xCD;
/// Poison for released memory in debug builds.
pub const FREED_BYTE: u8 = 0xDD;
/// Guard pattern for canary regions.
pub const CANARY_BYTE: u8 = 0xAB;
pub mod env;
