//! Packet-order regressions for a chest opened by a spell: its precast ends before its loot
//! response, and the earlier close must not reach the new window at the frame's drain.

use benilla_protocol::messages::{loot_type, ObjectFields};
use benilla_protocol::{EntityKind, SessionEvent};
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use super::call_tests::{client, feed, open, packets, row, seen, sent, Loot, CORPSE, LINEN};
use super::{LootLatch, LootMoveStart, LootState};
use crate::creature_anim::{apply_unit_edits, Precast, UnitEdit, UnitEdits};
use crate::net::{ClientCommand, Guid, GuidIndex, NetEntity, ObjectStore, SelfPlayer};

const CHEST: u64 = 0xF110_0000_0000_0042;
const OPENING: u32 = 3365;

fn chest_client() -> Loot {
    let mut l = client();
    let world = l.app.world_mut();
    let chest = world
        .spawn((
            Guid(CHEST),
            NetEntity {
                kind: EntityKind::GameObject,
                display_id: None,
                scale: 1.0,
            },
            ObjectStore(ObjectFields::from_pairs(&[(2, 0x21), (21, 3)])),
            Transform::default(),
            GlobalTransform::default(),
        ))
        .id();
    world.resource_mut::<GuidIndex>().0.insert(CHEST, chest);
    l
}

/// The same end that a matched `SMSG_SPELL_GO` queues and the net dispatcher applies before
/// the following packet (`0x6e7fc5` → `0x60d040` → `0x60d10b`).
fn finish_opening(l: &mut Loot) {
    let world = l.app.world_mut();
    let me = world
        .query_filtered::<Entity, With<SelfPlayer>>()
        .single(world)
        .expect("the active player");
    world.entity_mut(me).insert(Precast {
        spell_id: OPENING,
        busy: true,
    });
    world.resource::<UnitEdits>().push(UnitEdit::EndPrecast {
        entity: me,
        spell_id: Some(OPENING),
        tail: true,
    });
    apply_unit_edits(world);
}

fn chest_response(l: &mut Loot) {
    l.app.world_mut().resource_mut::<LootLatch>().0 = Some(CHEST);
    packets(
        l,
        vec![SessionEvent::LootResponse {
            guid: CHEST,
            loot_type: loot_type::PICKPOCKETING, // OPEN_LOCK's remapped LOOT_SKINNING.
            gold: 0,
            items: vec![row(0, LINEN)],
        }],
    );
}

fn drain(l: &mut Loot) {
    feed(l);
    l.app
        .world_mut()
        .run_system_once(super::drain_loot)
        .unwrap();
    feed(l);
}

#[test]
fn an_opening_spell_end_does_not_close_its_later_loot_response() {
    let mut l = chest_client();
    finish_opening(&mut l);
    chest_response(&mut l); // Same frame, after the spell's GO.
    drain(&mut l);
    assert_eq!(l.app.world().resource::<LootState>().source(), Some(CHEST));
    assert_eq!(seen(&mut l), "LOOT_OPENED");
    assert!(sent(&l).is_empty(), "no premature CMSG_LOOT_RELEASE");
    super::call_tests::click(&mut l, 1);
    assert!(matches!(
        sent(&l)[..],
        [ClientCommand::AutostoreLootItem { slot: 0 }]
    ));
}

#[test]
fn an_earlier_close_does_not_release_a_replacement_window() {
    let mut l = chest_client();
    open(&mut l, CORPSE, loot_type::CORPSE, 0, vec![row(0, LINEN)]);
    let _ = seen(&mut l);
    let _ = sent(&l);
    finish_opening(&mut l);
    chest_response(&mut l);
    drain(&mut l);
    assert_eq!(l.app.world().resource::<LootState>().source(), Some(CHEST));
    assert!(
        sent(&l).is_empty(),
        "an old close must not release the server's new loot"
    );
}

#[test]
fn a_spell_end_after_the_response_still_closes_that_window() {
    let mut l = chest_client();
    chest_response(&mut l);
    feed(&mut l);
    let _ = seen(&mut l);
    finish_opening(&mut l);
    drain(&mut l);
    assert_eq!(l.app.world().resource::<LootState>().source(), None);
    assert_eq!(seen(&mut l), "LOOT_CLOSED");
    assert!(matches!(
        sent(&l)[..],
        [ClientCommand::LootRelease { guid: CHEST }]
    ));
}

#[test]
fn a_new_response_rechecks_movement_instead_of_reusing_an_old_close() {
    use crate::creature_anim::move_flags::FORWARD;
    for (flags, stays) in [(0, true), (FORWARD, false)] {
        let mut l = chest_client();
        l.app
            .insert_resource(crate::player::Player::with_move_flags(flags));
        l.app.world_mut().resource_mut::<LootMoveStart>().0 = true;
        chest_response(&mut l);
        drain(&mut l);
        assert_eq!(
            l.app.world().resource::<LootState>().source().is_some(),
            stays
        );
        assert_eq!(
            sent(&l)
                .iter()
                .any(|c| matches!(c, ClientCommand::LootRelease { guid: CHEST })),
            !stays,
        );
    }
}
