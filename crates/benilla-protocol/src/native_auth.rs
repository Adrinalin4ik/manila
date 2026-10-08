//! Native login build selection for vanilla and Turtle-derived realms.

use anyhow::{bail, Context, Result};

/// The first realmd login build on native targets: stock 1.12.1 (5875).
///
/// Version rejection retries 7272, then 12340; a nonempty `WOW_REALMD_BUILD` pins one attempt.
/// On wasm [`crate::REALMD_BUILD`] is instead the fixed upstream build 12340.
/// World auth always uses [`crate::CLIENT_BUILD`], independently of login build selection.
pub const REALMD_BUILD: u16 = super::CLIENT_BUILD;

// Tortoise 1.18.1 requires 7272 to match its realm build. 12340 is the upstream's measured
// compatibility build for custom strict-version realms (see `lib.rs`).
const LOGIN_BUILDS: [u16; 3] = [REALMD_BUILD, 7272, 12340];
const VERSION_INVALID: u8 = 0x09;
const VERSION_UPDATE: u8 = 0x0a;

fn build_from_override(value: Option<&str>) -> Result<Option<u16>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let build = value
        .parse::<u16>()
        .context("WOW_REALMD_BUILD must be a positive 16-bit build number")?;
    if build == 0 {
        bail!("WOW_REALMD_BUILD must be a positive 16-bit build number");
    }
    Ok(Some(build))
}

pub(super) async fn logon_async(
    host: &str,
    username: &str,
    password: &str,
) -> Result<super::Logon> {
    let value = match std::env::var("WOW_REALMD_BUILD") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error).context("reading WOW_REALMD_BUILD"),
    };
    logon_with_override(host, username, password, value.as_deref()).await
}

async fn logon_with_override(
    host: &str,
    username: &str,
    password: &str,
    value: Option<&str>,
) -> Result<super::Logon> {
    let pinned = build_from_override(value)?;
    let builds = pinned.as_slice();
    let builds = if builds.is_empty() {
        &LOGIN_BUILDS
    } else {
        builds
    };
    for (index, &build) in builds.iter().enumerate() {
        let result = super::logon_with_build(host, username, password, build).await;
        // Version rejection (with or without a patch offer) permits another build. Account errors,
        // transport failures, invalid server proofs and offline realm flags stay authoritative.
        let version_rejected = result.as_ref().err().is_some_and(|error| {
            error
                .downcast_ref::<super::AuthReject>()
                .is_some_and(|reject| matches!(reject.code, VERSION_INVALID | VERSION_UPDATE))
        });
        if !version_rejected || index + 1 == builds.len() {
            return result;
        }
    }
    unreachable!("at least one login build")
}

#[cfg(test)]
#[path = "native_auth_tests.rs"]
mod tests;
