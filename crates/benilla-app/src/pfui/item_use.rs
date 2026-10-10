//! Direct addon item use reuses the stock cast and bind gates without vendor/bank click modes.

use super::*;
use crate::spell::cast_target::CastTargeting;

struct PendingTarget {
    item: u64,
    unit: Option<u64>,
    auto_self_cast: bool,
    session: u64,
}

/// Lives beside the stock pending use, so replacing the VM cannot lose its ownership marker.
#[derive(Resource, Default)]
pub(crate) struct DeferredItemUse {
    pending: Option<PendingTarget>,
    serial: u64,
}

impl DeferredItemUse {
    pub(crate) fn clear(&mut self) {
        self.pending = None;
        self.serial = self.serial.wrapping_add(1);
    }
}

fn position(
    store: &benilla_protocol::ObjectFields,
    objects: &Objects,
    guid: u64,
) -> Option<(i64, u32)> {
    for bag in [0, 1, 2, 3, 4, -2, EQUIPMENT_BAG] {
        let count = match bag {
            0 => 16,
            -2 => 16,
            EQUIPMENT_BAG => 19,
            _ => 36,
        };
        for slot in 0..count {
            if slot_guid(store, bag, slot, objects) == Some(guid) {
                return Some((bag, u32::from(slot) + 1));
            }
        }
    }
    None
}

fn resolved(ladder: &crate::spell::CastLadder, guid: u64) -> Option<Clicked> {
    let obj = ladder.objects.object(guid)?;
    let entry = obj.object_entry()?;
    let t = ladder.items.template(entry, guid, &ladder.commands)?;
    Some(Clicked {
        guid,
        entry,
        inventory_type: t.inventory_type,
        display_info_id: t.display_info_id,
        item_text_id: obj.item_text_id().unwrap_or(0),
        start_quest: t.start_quest,
        spell_index: t.use_spell_index().unwrap_or(0),
        use_spell: t.use_spell.map(|u| u.spell_id),
        unwraps_gift: t.unwraps_gift(obj.item_flags().unwrap_or(0)),
        begins_gift_wrap: t.begins_gift_wrap(obj.item_flags().unwrap_or(0)),
        opens_loot: t.opens_loot(),
        page_text: t.page_text,
        is_charter: t.flags & benilla_protocol::messages::ITEM_FLAG_CHARTER != 0,
    })
}

fn request(c: Clicked, bag: i64, slot: u32) -> Option<super::super::ItemUse> {
    let (bag_index, slot) = wire_pos(bag, slot)?;
    Some(super::super::ItemUse {
        guid: Some(c.guid),
        start_quest: c.start_quest,
        bag_index,
        slot,
        entry: c.entry,
        spell_index: c.spell_index,
        use_spell: c.use_spell,
        on_object: None,
        is_charter: c.is_charter,
    })
}

impl ScriptItemUse<'_, '_> {
    pub(crate) fn pfui_use_item(&mut self, script: &mut UiScript, guid: u64, unit: Option<u64>) {
        let Some(ctx) = (match unit {
            Some(unit) => self.targeting.pfui_context(unit),
            None => Some(self.targeting.context()),
        }) else {
            return;
        };
        let Some((bag, slot)) = self
            .self_q
            .iter()
            .next()
            .and_then(|store| position(&store.0, &self.ladder.objects, guid))
        else {
            return;
        };
        if self.pending_items.contains(bag, slot) {
            return;
        }
        let Some(c) = resolved(&self.ladder, guid) else {
            return;
        };
        let Some(it) = request(c, bag, slot) else {
            return;
        };
        if c.unwraps_gift {
            let _ = self.ladder.commands.0.send(ClientCommand::OpenItem {
                bag_index: it.bag_index,
                slot: it.slot,
            });
            return;
        }
        if c.begins_gift_wrap {
            script.arm_gift_wrap(bag, slot);
            return;
        }
        if c.start_quest == 0 {
            if c.page_text != 0 || c.item_text_id != 0 {
                if !self.item_text.toggle_closed(guid) {
                    if c.page_text != 0 {
                        self.item_text.open_pages(guid)
                    } else {
                        self.item_text.open_letter(guid, c.item_text_id)
                    }
                }
                return;
            }
            if c.opens_loot {
                self.loot_latch.0 = Some(guid);
                let (_, count) = self
                    .self_q
                    .iter()
                    .next()
                    .map(|store| slot_guid_count(Some(store), bag, slot, &self.ladder.objects))
                    .unwrap_or((0, 0));
                self.pending_items.add([(bag, slot, guid, count)]);
                script.fire_event("ITEM_LOCK_CHANGED", Vec::new());
                let _ = self.ladder.commands.0.send(ClientCommand::OpenItem {
                    bag_index: it.bag_index,
                    slot: it.slot,
                });
                return;
            }
        }
        let serial = self.gate.pfui_item_use.as_ref().map(|state| state.serial);
        super::super::send_item_use(
            it,
            &ctx,
            &mut self.ladder,
            script,
            &mut self.gate,
            false,
            &mut self.ui_errors,
        );
        let deferred = self.gate.pfui_item_use.as_ref().map(|state| state.serial) != serial;
        if deferred
            && self
                .gate
                .on_use
                .0
                .is_some_and(|pending| pending.guid == Some(guid))
        {
            if let Some(state) = self.gate.pfui_item_use.as_mut() {
                state.pending = Some(PendingTarget {
                    item: guid,
                    unit: ctx.selection_guid,
                    auto_self_cast: ctx.auto_self_cast,
                    session: script.session(),
                });
            }
        }
    }
}

/// Returns true when this was an extension-owned target, including a refusal after disappearance.
pub(super) fn confirm(
    script: &mut UiScript,
    it: super::super::ItemUse,
    targeting: &CastTargeting,
    ladder: &mut crate::spell::CastLadder,
    gate: &mut crate::ui_bind_confirm::BindGate,
    errors: &mut crate::ui_action::UiErrorKeys,
    pending_items: &PendingItemOps,
) -> bool {
    let Some(pending) = gate
        .pfui_item_use
        .as_mut()
        .and_then(|state| state.pending.take())
    else {
        return false;
    };
    if it.guid != Some(pending.item) {
        return false;
    }
    if pending.session != script.session() {
        return true;
    }
    let mut ctx = match pending.unit {
        Some(unit) => {
            let Some(ctx) = targeting.pfui_context(unit) else {
                return true;
            };
            ctx
        }
        None => {
            let mut ctx = targeting.context();
            ctx.selection_guid = None;
            ctx.rel.target_store = None;
            ctx.rel.target_owner_store = None;
            ctx.rel.group.target_guid = None;
            ctx.range.target_pos = None;
            ctx.range.target = None;
            ctx
        }
    };
    ctx.auto_self_cast = pending.auto_self_cast;
    let Some((bag, slot)) = ctx
        .rel
        .self_store
        .and_then(|store| position(&store.0, &ladder.objects, pending.item))
    else {
        return true;
    };
    if pending_items.contains(bag, slot) {
        return true;
    }
    let Some(c) = resolved(ladder, pending.item) else {
        return true;
    };
    let Some(current) = request(c, bag, slot) else {
        return true;
    };
    // Moving the same instance is valid; transforming it does not inherit the old consent.
    if current.entry != it.entry
        || current.use_spell != it.use_spell
        || current.spell_index != it.spell_index
        || current.start_quest != it.start_quest
    {
        return true;
    }
    super::super::send_item_use(current, &ctx, ladder, script, gate, true, errors);
    true
}

#[cfg(test)]
#[path = "item_use_tests.rs"]
mod tests;
