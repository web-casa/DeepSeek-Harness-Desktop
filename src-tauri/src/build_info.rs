//! Compile-time distribution flavor, driven by the `STORE_BUILD` environment
//! variable set in the MSIX CI job. Normal website builds leave it unset.

pub const STORE_BUILD: bool = cfg!(store_build);
pub const DISTRIBUTION: &str = if STORE_BUILD { "store" } else { "web" };
