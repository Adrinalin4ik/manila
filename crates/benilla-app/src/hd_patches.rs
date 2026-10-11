//! The `hdPatches` setting: whether the lettered patch archives (`patch-A.mpq` to `patch-Z.mpq`,
//! where player-installed art packs go) are part of the patch chain.
//!
//! The chain is opened once and everything read from it is cached for the life of the process, so
//! the setting is read here before the first archive opens and a change applies at the next
//! start. [`relaunch_if_requested`] makes that start one write away: the session-only
//! `restartClient` CVar quits, and the launcher starts the same binary again with the same
//! arguments.

use std::sync::atomic::{AtomicBool, Ordering};

use bevy::prelude::*;

/// Set by a write of 1 to `restartClient`: start the client again once it has shut down.
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

/// `restartClient 1`: quit now, and [`relaunch_if_requested`] does the rest after the app has
/// run down.
pub(crate) fn on_cvar(ev: On<crate::cvars::CvarChanged>, mut exit: MessageWriter<AppExit>) {
    if ev.is("restartClient") && ev.flag() {
        info!("restart: requested by the restartClient CVar");
        RESTART_REQUESTED.store(true, Ordering::Relaxed);
        exit.write(AppExit::Success);
    }
}

/// The saved value of a CVar in `config.toml`'s `[cvars]` table, read without the registry: this
/// runs before the app exists.
fn saved_cvar(text: &str, name: &str) -> Option<String> {
    let mut in_cvars = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_cvars = line == "[cvars]";
            continue;
        }
        if !in_cvars {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// `"0"` turns the archives off; anything else, and no value, leaves them on.
fn mounts(value: Option<&str>) -> bool {
    value.map(str::trim) != Some("0")
}

/// Set the chain's switch from `$WOW_HD_PATCHES`, else from the saved `hdPatches`.
pub(crate) fn apply_saved() {
    let saved = std::env::var("WOW_HD_PATCHES").ok().or_else(|| {
        let text = std::fs::read_to_string(crate::local_state::config_path()?).ok()?;
        saved_cvar(&text, "hdPatches")
    });
    let mount = mounts(saved.as_deref());
    benilla_formats::set_letter_patches(mount);
    if !mount {
        // Before the logger exists, so straight to the console.
        eprintln!("hd patches: off, the lettered patch archives are not mounted");
    }
}

/// Start the client again if `restartClient` asked for it. Called after the app has run down,
/// so the settings and saved variables are on disk before the new process reads them.
pub(crate) fn relaunch_if_requested() {
    if !RESTART_REQUESTED.swap(false, Ordering::Relaxed) {
        return;
    }
    // A scripted run never restarts itself: its lifetime is the harness's.
    if std::env::var_os("WOW_UNATTENDED").is_some() {
        eprintln!("restart: requested, and not done in an unattended run");
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut again = std::process::Command::new(exe);
    again.args(std::env::args_os().skip(1));
    if let Err(e) = again.spawn() {
        eprintln!("restart: could not start the client again: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_value_is_read_from_the_cvars_table_only() {
        let text =
            "# note\n[other]\nhdPatches = \"0\"\n[cvars]\nbloom = \"0\"\nhdPatches = \"0\"\n";
        assert_eq!(saved_cvar(text, "hdPatches").as_deref(), Some("0"));
        assert_eq!(saved_cvar(text, "bloom").as_deref(), Some("0"));
        assert_eq!(
            saved_cvar("[other]\nhdPatches = \"0\"\n", "hdPatches"),
            None
        );
        assert_eq!(
            saved_cvar("[cvars]\nrealmName = \"A = B\"\n", "realmName").as_deref(),
            Some("A = B")
        );
    }

    #[test]
    fn only_a_saved_zero_turns_the_archives_off() {
        assert!(mounts(None));
        assert!(mounts(Some("1")));
        assert!(mounts(Some("")));
        assert!(!mounts(Some("0")));
        assert!(!mounts(Some(" 0 ")));
    }
}
