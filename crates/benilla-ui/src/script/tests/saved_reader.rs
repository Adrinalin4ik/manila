//! wenilla carry: saved variables come back through the host's reader where it seated one, as the
//! browser has no filesystem for `std::fs::read`.

use std::path::{Path, PathBuf};

use crate::script::{AddOnInfo, UiScript};

#[test]
fn saved_variables_load_through_the_hosts_reader() {
    let mut s = UiScript::new().unwrap();
    s.register_addons(
        vec![AddOnInfo {
            name: "Probe".into(),
            interface: 11200,
            enabled: true,
            saved_variables: vec!["ProbeDB".into()],
            ..Default::default()
        }],
        None,
        // A directory no filesystem holds: only the reader can answer for it.
        Some(PathBuf::from("/benilla-config/saved")),
        None,
    );
    s.set_addon_saved_reader(Box::new(|path: &Path| {
        if path == Path::new("/benilla-config/saved/Probe.lua") {
            Ok(b"ProbeDB = 7".to_vec())
        } else {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }));
    s.load_addon_saved_variables("Probe");
    assert_eq!(s.eval::<i64>("return ProbeDB").unwrap(), 7);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
