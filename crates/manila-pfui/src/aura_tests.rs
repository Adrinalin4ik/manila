use benilla_ui::script::{
    pfui_host::{
        auras::{self, Aura},
        units,
    },
    AuraState, UiScript, UnitGuids, UnitState,
};

fn fixture() -> UiScript {
    let mut script = UiScript::new().unwrap();
    crate::install(&script).unwrap();
    script.set_unit_guids(&UnitGuids {
        player: 3,
        pet: 4,
        target: 5,
        ..Default::default()
    });
    script
}

fn aura(slot: u8, spell: u32, helpful: bool, crowd_control: bool) -> Aura {
    Aura {
        slot,
        crowd_control,
        source_guid: None,
        state: AuraState {
            spell_id: spell,
            name: Some(format!("Aura{spell}")),
            helpful,
            ..Default::default()
        },
    }
}

#[test]
fn crowd_control_filters_cover_indexed_and_buffered_queries_including_overflow_and_negation() {
    let script = fixture();
    let mut stun = aura(32, 1, false, true);
    stun.source_guid = Some(4);
    stun.state.debuff_type = Some("Magic".into());
    auras::set_auras(
        script.lua(),
        5,
        Some(vec![
            stun,
            aura(5, 2, false, true),
            aura(33, 3, false, false),
            aura(0, 4, true, false),
        ]),
    );
    script.run(r#"
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'CROWD_CONTROL').spellId==1)
        assert(C_UnitAuras.GetAuraDataByIndex('target',2,'HARMFUL|CROWD_CONTROL').spellId==2)
        assert(C_UnitAuras.GetAuraDataByIndex('target',3,'HARMFUL|CROWD_CONTROL')==nil)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'!HELPFUL !CROWD_CONTROL').spellId==3)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'HARMFUL|PLAYER|CROWD_CONTROL|DISPELLABLE').spellId==1)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'HARMFUL|!PLAYER|CROWD_CONTROL').spellId==2)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'helpful|!crowd_control').spellId==4)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'HELPFUL|CROWD_CONTROL')==nil)
        local slots={}
        local token,count=C_UnitAuras.GetAuraSlots('target','CROWD_CONTROL',1,nil,slots)
        assert(token~=nil and count==1 and slots[1]==32)
        token,count=C_UnitAuras.GetAuraSlots('target','CROWD_CONTROL',1,token,slots)
        assert(token==nil and count==1 and slots[1]==5)
        assert(C_UnitAuras.UnitAuraBySlot('target',slots[1])=='Aura2')
        assert(C_UnitAuras.GetAuraDataBySlot('target',slots[1]).isHarmful)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'HELPFUL|CROWD_CONTROL_EXTRA').spellId==4)
    "#).unwrap();
}

#[test]
fn observed_ownership_refreshes_are_readable_during_events_and_survive_unchanged_snapshots() {
    let script = fixture();
    let incoming = aura(32, 1, false, false);
    auras::set_auras(script.lua(), 5, Some(vec![incoming.clone()]));
    script
        .run(
            r#"
        updates=0
        local f=CreateFrame('Frame'); f:RegisterUnitEvent('UNIT_AURA','target')
        f:SetScript('OnEvent',function(self,event,unit)
            updates=updates+1
            current=C_UnitAuras.GetAuraDataByIndex(unit,1,'HARMFUL|PLAYER')
        end)
        assert(C_UnitAuras.GetAuraDataByIndex('target',1,'HARMFUL|PLAYER')==nil)
    "#,
        )
        .unwrap();
    auras::annotate(script.lua(), 5, 1, 3, 10.0, 12.0);
    script
        .run("assert(updates==1 and current.sourceUnit=='player' and current.expirationTime==12)")
        .unwrap();
    auras::set_auras(script.lua(), 5, Some(vec![incoming.clone()]));
    auras::annotate(script.lua(), 5, 1, 3, 10.0, 12.0);
    script.run("assert(updates==1 and C_UnitAuras.GetAuraDataBySlot('target',32).sourceUnit=='player')").unwrap();
    auras::annotate(script.lua(), 5, 1, 3, 15.0, 20.0);
    script
        .run("assert(updates==2 and current.duration==15 and current.expirationTime==20)")
        .unwrap();
    auras::set_auras(script.lua(), 5, None);
    auras::set_auras(script.lua(), 5, Some(vec![incoming]));
    script
        .run("assert(C_UnitAuras.GetAuraDataBySlot('target',32).sourceGUID==nil)")
        .unwrap();
}

#[test]
fn player_duration_packets_remain_authoritative_and_sources_resolve_through_focus() {
    let mut script = fixture();
    let caster = UnitState {
        guid: 99,
        exists: true,
        has_object: true,
        ..Default::default()
    };
    units::set_unit_states(script.lua(), [(99, caster.clone(), 0)]);
    script.set_unit_by_guid(99, Some(caster));
    script.run("FocusUnit('0x0000000000000063')").unwrap();
    let mut player = aura(0, 1, true, false);
    player.state.duration = 55.0;
    player.state.expiration_time = 60.0;
    auras::set_auras(script.lua(), 3, Some(vec![player.clone()]));
    auras::annotate(script.lua(), 3, 1, 99, 10.0, 12.0);
    script
        .run(
            r#"
        local a=C_UnitAuras.GetAuraDataBySlot('player',0)
        assert(a.duration==55 and a.expirationTime==60 and a.sourceUnit=='focus')
        assert(a.sourceGUID=='0x0000000000000063')
    "#,
        )
        .unwrap();
    script
        .run(
            r#"
        durationUpdates=0
        local f=CreateFrame('Frame');f:RegisterUnitEvent('UNIT_AURA','player')
        f:SetScript('OnEvent',function()
            durationUpdates=durationUpdates+1
            assert(C_UnitAuras.GetAuraDataBySlot('player',0).expirationTime==70)
        end)
    "#,
        )
        .unwrap();
    player.state.expiration_time = 70.0;
    auras::set_auras(script.lua(), 3, Some(vec![player.clone()]));
    player.state.expiration_time += 1e-10;
    auras::set_auras(script.lua(), 3, Some(vec![player]));
    script.run("assert(durationUpdates==1)").unwrap();
    script.set_unit_guids(&UnitGuids {
        player: 3,
        party_pets: [99, 0, 0, 0],
        ..Default::default()
    });
    script
        .run("assert(C_UnitAuras.GetAuraDataBySlot('player',0).sourceUnit=='partypet1')")
        .unwrap();
}
