//! Source audits must cover the same optional modules as the tested client.

pub(super) fn is_compiled(path: &std::path::Path) -> bool {
    let rel = super::rel_path(path);
    cfg!(feature = "pfui")
        || !(rel.starts_with("pfui/")
            || matches!(
                rel.as_str(),
                "pfui.rs"
                    | "net/pfui.rs"
                    | "target/pfui.rs"
                    | "ui_aura/pfui.rs"
                    | "ui_items/pfui.rs"
                    | "ui_unit/pfui.rs"
            ))
}

#[test]
fn optional_sources_follow_the_build_feature() {
    for path in [
        "pfui.rs",
        "pfui/nampower.rs",
        "net/pfui.rs",
        "ui_unit/pfui.rs",
    ] {
        assert_eq!(
            is_compiled(&super::src_dir().join(path)),
            cfg!(feature = "pfui")
        );
    }
    assert!(is_compiled(&super::src_dir().join("spell/cooldowns.rs")));
}
