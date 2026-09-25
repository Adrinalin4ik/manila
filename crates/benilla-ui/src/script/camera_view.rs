//! The camera-view verbs from the reference's Lua table at `0x84f7a0`: `SetView`, `SaveView`,
//! `ResetView`, `NextView`, `PrevView` and `FlipCameraYaw`. Each acts on engine state the VM does
//! not hold, so each call queues a [`CameraViewRequest`] for the app's camera rig.
//!
//! The indexed verbs share one prologue and return silently, never reaching `luaL_error`
//! (`0x6f4940`), on anything but a number that truncates to 1 to 5; `FlipCameraYaw` keeps its
//! argument's fraction. `SaveView(1)` and `ResetView(1)` act on first person (`0x50fa30` has no
//! view-0 gate; only `Bindings.xml` lacks the bindings). `NextView` and `PrevView` stop at the
//! ends and never wrap (`0x50faa0`, `0x50fac0`).

use mlua::{Lua, Value};

use super::Model;

/// The number of camera views, the rows of the reference's default-string table `0x84f488`.
pub const CAMERA_VIEW_COUNT: u8 = 5;

/// A camera-view intent for the app. Indices are internal, `0..CAMERA_VIEW_COUNT`, range-checked
/// here where the reference checks them, in the Lua handler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraViewRequest {
    /// `SetView(n)`: make view `n-1` live (distance, pitch and the saved yaw offset).
    Set(u8),
    /// `SaveView(n)`: store the live pose into view `n-1`.
    Save(u8),
    /// `ResetView(n)`: put view `n-1` back to its shipped default.
    Reset(u8),
    /// `NextView()`: the view above the current one, if any.
    Next,
    /// `PrevView()`: the view below the current one, if any.
    Prev,
    /// `FlipCameraYaw(degrees)`: add degrees to the camera's yaw; `Bindings.xml:795` passes 180.
    FlipYaw(f32),
    /// `CameraZoomIn(amount)` — step the orbit target this many yards toward the head, the same
    /// move a wheel notch makes (the app side's `CAM_ZOOM_STEP`). The rig glides the realized
    /// distance there at `cameraDistanceMoveSpeed`.
    ZoomIn(f32),
    /// `CameraZoomOut(amount)` — [`Self::ZoomIn`]'s twin, away from the head.
    ZoomOut(f32),
}

impl super::UiScript {
    /// Drain the camera-view intents queued since the last call.
    pub fn take_camera_view_requests(&mut self) -> Vec<CameraViewRequest> {
        std::mem::take(&mut self.model_mut().camera_view_requests)
    }
}

/// The indexed verbs' shared prologue: the internal index, or `None` for a silent return.
fn view_arg(lua: &Lua, v: Value) -> Option<u8> {
    // Is-number (`0x6f34d0`) and `tonumber` (`0x6f3620`) take a numeric string; the cast
    // truncates toward zero, as `0x40a2b0` does.
    let n = lua.coerce_number(v).ok().flatten()? as i64 as i32;
    (1..=i32::from(CAMERA_VIEW_COUNT))
        .contains(&n)
        .then(|| (n - 1) as u8)
}

/// Register the six camera-view globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // SetView(n), `0x50b5b0`. The reference's `0x512e90` also writes the `cameraView` CVar, so
    // the view survives a restart; the app does both.
    g.set(
        "SetView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Set(view));
            }
            Ok(())
        })?,
    )?;

    // SaveView(n), `0x50b600` → `0x50fa30`: the camera's target distance, pitch and yaw go into
    // the slot and each to its archived CVar.
    g.set(
        "SaveView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Save(view));
            }
            Ok(())
        })?,
    )?;

    // ResetView(n), `0x50b640` → `0x50fae0`: re-parses the view's default string (`0x84f488`)
    // into the slot and re-applies the view if it is live.
    g.set(
        "ResetView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Reset(view));
            }
            Ok(())
        })?,
    )?;

    // NextView() and PrevView(), `0x50b680` and `0x50b690`: neither reads the Lua stack.
    g.set(
        "NextView",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.camera_view_requests.push(CameraViewRequest::Next);
            Ok(())
        })?,
    )?;
    g.set(
        "PrevView",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.camera_view_requests.push(CameraViewRequest::Prev);
            Ok(())
        })?,
    )?;

    // FlipCameraYaw(degrees), `0x50b6a0`: adds `degrees × π/180` to the camera's yaw, fraction
    // kept; a non-number returns silently.
    g.set(
        "FlipCameraYaw",
        lua.create_function(|lua, degrees: Value| {
            if let Some(d) = lua.coerce_number(degrees).ok().flatten() {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::FlipYaw(d as f32));
            }
            Ok(())
        })?,
    )?;

    // CameraZoomIn(amount) — the wheel-notch verb as its own Lua entry point: the same move the
    // CAMERAZOOMIN binding dispatch makes ("1.12's own `CameraZoomIn(1.0)` argument" — the
    // notch's own step, VERIFIED 1.0 yd in `WoW.exe`, the app side's `CAM_ZOOM_STEP`). The
    // amount keeps its fraction; an absent or non-number argument is the 1.0 notch.
    g.set(
        "CameraZoomIn",
        lua.create_function(|lua, amount: Value| {
            let amount = lua.coerce_number(amount).ok().flatten().unwrap_or(1.0) as f32;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model
                .camera_view_requests
                .push(CameraViewRequest::ZoomIn(amount));
            Ok(())
        })?,
    )?;

    // CameraZoomOut(amount) — [`CameraZoomIn`]'s twin, away from the head. TWoW's barbershop is
    // the chain's only caller (`SetupCamera`: SetView, FlipCameraYaw, then out two steps).
    g.set(
        "CameraZoomOut",
        lua.create_function(|lua, amount: Value| {
            let amount = lua.coerce_number(amount).ok().flatten().unwrap_or(1.0) as f32;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model
                .camera_view_requests
                .push(CameraViewRequest::ZoomOut(amount));
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CameraViewRequest;
    use crate::script::UiScript;

    /// The zoom pair queues the amount given, and a bare call steps the notch's own 1.0 — the
    /// number `WoW.exe`'s zoom verb is VERIFIED to default (the app side's `CAM_ZOOM_STEP`).
    #[test]
    fn camera_zoom_queues_the_step_and_defaults_to_one() {
        let mut s = UiScript::new().unwrap();
        s.run("CameraZoomOut(2)").unwrap();
        s.run("CameraZoomIn(0.5)").unwrap();
        s.run("CameraZoomIn()").unwrap();
        s.run("CameraZoomOut('garbage')").unwrap();
        assert_eq!(
            s.take_camera_view_requests(),
            vec![
                CameraViewRequest::ZoomOut(2.0),
                CameraViewRequest::ZoomIn(0.5),
                CameraViewRequest::ZoomIn(1.0),
                CameraViewRequest::ZoomOut(1.0),
            ]
        );
        assert!(s.take_camera_view_requests().is_empty());
    }
}
