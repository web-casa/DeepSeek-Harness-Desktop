//! Distribution flavor: `STORE_BUILD` is compiled into the MSIX payload, while
//! Snap is recognized from the package-owned runtime markers. Normal website
//! builds have neither marker.

pub const STORE_BUILD: bool = cfg!(store_build);
const SNAP_NAME: &str = "dsh-desktop-community";

fn is_our_snap_runtime(snap_root: Option<&str>, snap_name: Option<&str>) -> bool {
    snap_root.is_some_and(|path| !path.is_empty()) && snap_name == Some(SNAP_NAME)
}

pub fn is_snap_runtime() -> bool {
    let snap_root = std::env::var("SNAP").ok();
    let snap_name = std::env::var("SNAP_NAME").ok();
    is_our_snap_runtime(snap_root.as_deref(), snap_name.as_deref())
}

pub fn distribution() -> &'static str {
    if STORE_BUILD {
        "store"
    } else if is_snap_runtime() {
        "snap"
    } else {
        "web"
    }
}

#[cfg(test)]
mod tests {
    use super::is_our_snap_runtime;

    #[test]
    fn snap_runtime_requires_the_nonempty_package_owned_markers() {
        assert!(!is_our_snap_runtime(None, None));
        assert!(!is_our_snap_runtime(
            Some(""),
            Some("dsh-desktop-community")
        ));
        assert!(!is_our_snap_runtime(
            Some("/snap/dsh-desktop-community/x1"),
            None
        ));
        assert!(!is_our_snap_runtime(
            Some("/snap/other/x1"),
            Some("other-untrusted-snap")
        ));
        assert!(is_our_snap_runtime(
            Some("/snap/dsh-desktop-community/x1"),
            Some("dsh-desktop-community")
        ));
    }
}
