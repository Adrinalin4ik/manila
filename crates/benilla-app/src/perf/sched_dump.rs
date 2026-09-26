//! **What is actually IN each phase tile** — `/console dumpSchedule 1`.
//!
//! The journal's twelve tiles are exact boundaries but vague labels, and the gap has now cost two
//! rounds. `u_stream` is not the Stream stage: `WorldStage::*` are sets systems opt into with
//! `.in_set(..)`, and every Update system that opts into none floats between the marks. The proof
//! is in journal 42, the cleanest capture this investigation produced — 71 s, network off, no
//! loading — where `u_net` read **4.76 ms with the network switched off entirely**, and
//! `u_stream` read 12.14 ms, the single largest item in a 50.26 ms frame, with the streamer doing
//! nothing at all.
//!
//! So the biggest line in the budget is a tile whose contents nobody has ever listed. This lists
//! them: every system of `Update` and `PostUpdate` in the order the executor runs them, with the
//! phase marks shown in place, so each tile's membership can be read off directly.
//!
//! **Not a profiler.** It costs nothing per frame - it is a one-shot walk of an already-built
//! schedule, run through `run_system_cached` when the CVar is set, registered nowhere. Per-system
//! TIMING still needs a cheaper mechanism than bevy's `trace`, which took the frame from 65 ms to
//! 535-642 and made the client unusable. Naming is the half that is free, and it is the half that
//! turns "12 ms somewhere in Update" into a list to read.

use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;

/// Dump `Update` and `PostUpdate` in execution order. Exclusive, because the schedules live in the
/// `World` and the order is only knowable after they are built.
pub(crate) fn dump_schedule(world: &mut World) {
    for label in [
        bevy::app::Update.intern(),
        bevy::app::PostUpdate.intern(),
    ] {
        let mut out: Vec<String> = Vec::new();
        world.resource_scope(|_w, schedules: Mut<bevy::ecs::schedule::Schedules>| {
            let Some(schedule) = schedules.get(label) else {
                return;
            };
            match schedule.systems() {
                Ok(iter) => {
                    for (i, (_key, system)) in iter.enumerate() {
                        let name = system.name().to_string();
                        // The type path is most of every name and none of the information; the
                        // marks keep their full spelling so a tile boundary is unmistakable.
                        let short = if name.contains("phase_mark") || name.contains("rmark") {
                            format!(">>> {name}")
                        } else {
                            name.rsplit("::").next().unwrap_or(&name).to_string()
                        };
                        out.push(format!("{i:>4} {short}"));
                    }
                }
                Err(_) => out.push("  (schedule not initialized - run a frame first)".into()),
            }
        });
        info!("[sched] ===== {label:?}: {} systems, in run order =====", out.len());
        for line in out {
            info!("[sched] {line}");
        }
    }
}
