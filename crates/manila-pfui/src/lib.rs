//! Opt-in building blocks for the ClassicAPI edition of pfUI.
//!
//! This is an extension, not a claim of ClassicAPI parity. The caller explicitly installs it
//! on each in-game VM, before loading third-party addons. It publishes the ClassicAPI version
//! pfUI's load gate requires, beside a table that marks the surface as emulated and partial.
//! The headless audit is separate from the client and does not install or modify addons.

mod attributes;
pub mod bags;
pub mod bubbles;
pub mod clock;
pub mod creature;
mod encoding;
pub mod icons;
pub mod loot;
mod lua51;
pub mod maps;
pub mod nampower;
pub mod player_cache;
pub mod spell;
pub mod swing;
mod widgets;

#[cfg(feature = "audit")]
pub mod audit;

use benilla_ui::script::UiScript;
use mlua::Value;
pub use mlua::{Function as LuaFunction, Lua};

const INSTALLED: &str = "manila.pfui.installed";

/// Install the independently implemented compatibility subset, once per VM. Default clients
/// never call this. No dummy engine API is published; `ManilaClassicAPICompatibility` states
/// that the surface behind `CLASSIC_API_VERSION` is partial.
pub fn install(script: &UiScript) -> mlua::Result<()> {
    let lua = script.lua();
    if lua.named_registry_value::<Option<bool>>(INSTALLED)? == Some(true) {
        return Ok(());
    }
    for name in [
        "C_Timer",
        "C_EncodingUtil",
        "C_ColorUtil",
        "Mixin",
        "SafePack",
        "GameTooltip_Hide",
        "tInvert",
        "CreateFromMixins",
        "CreateAndInitFromMixin",
        "IconDataProviderMixin",
        "IconDataProviderExtraType",
        "IconDataProviderIconType",
        "IconDataProvider_GetAllIconTypes",
        "GetMacroIcons",
        "GetMacroItemIcons",
        "GetLooseMacroIcons",
        "GetLooseMacroItemIcons",
        "GetCursorInfo",
        "INVSLOTS_EQUIPABLE_IN_COMBAT",
        "HEARTHSTONE_ITEM_ID",
        "C_LootHistory",
        "C_ChatBubbles",
        "C_Map",
        "UnitCreatureTypeID",
        "UnitCreatedBySpell",
        "ColorMixin",
        "CreateColor",
        "CallbackRegistryMixin",
        "EventUtil",
        "EventRegistry",
        "RunNextFrame",
        "hooksecurefunc",
        "UnitClassBase",
        "Enum",
        "C_DateAndTime",
        "UnitGUID",
        "UnitTokenFromGUID",
        "C_Item",
        "C_Container",
        "C_PlayerInfo",
        "C_AddOns",
        "C_FriendList",
        "C_UnitAuras",
        "C_PlayerCache",
        "C_Macro",
        "C_Reputation",
        "C_QuestLog",
        "C_EquipmentSet",
        "IsPlayerGuid",
        "C_SpellBook",
        "UnitPower",
        "UnitPowerMax",
        "FocusUnit",
        "ClearFocus",
        "IsInGroup",
        "IsInRaid",
        "UnitSubName",
        "C_NamePlate",
        "C_NewItems",
        "C_MerchantFrame",
        "IsSpellKnown",
        "UnitIsFeignDeath",
        "UnitIsMinion",
        "GetUnitField",
        "GetPlayerGuid",
        "GetNampowerVersion",
        "QueueSpellByName",
        "QueueScript",
        "CastSpellByNameNoQueue",
        "CastSpellNoQueue",
        "GetCastInfo",
        "GetWowTimeMs",
        "GetPlayerAuraDuration",
        "CancelPlayerAuraSpellId",
        "ManilaNampowerCompatibility",
        "GetTotemInfo",
        "GetShapeshiftFormID",
        "GetSpellInfo",
        "GetSpellBonusDamage",
        "UnitSpellHaste",
        "IsMounted",
        "IsStealthed",
        "Dismount",
        "CancelShapeshiftForm",
        "TargetTotem",
        "GetActionInfo",
        "GetInventoryItemID",
        "GetInventoryItemDurability",
        "ClassicAPI_ToggleUnitMenu",
        "Item",
        "ItemLocation",
        "Round",
        "Clamp",
        "CopyTable",
        "CreateObjectPool",
        "CreateTexturePool",
        "IsModifierKeyDown",
        "strtrim",
        "GetLootRollItemID",
        "GetLootSlotItemID",
        "GetMerchantItemID",
        "GetQuestItemID",
        "GetQuestLogItemID",
        "GetTradeSkillItemID",
        "GetTradeSkillReagentItemID",
        "GetCraftReagentItemID",
        "GetMacroSpell",
        "GetNumTrackingTypes",
        "GetTrackingInfo",
        "SetTracking",
        "FindSpellBookSlotByID",
        "GetFactionInfoByID",
        "UnitNameFromGUID",
        "GetPlayerInfoByGUID",
        "GetInventoryItemsForSlot",
        "EquipmentManager_UnpackLocation",
        "EquipmentManager_GetLocationData",
        "INTERFACE_VERSION",
        "CLASSIC_API_VERSION",
        "ManilaClassicAPICompatibility",
        "GetUnitSpeed",
        "GetInventoryItemRepairCost",
        "GetInboxItemLink",
        "GetInboxItemID",
        "GetSendMailItemLink",
        "CreateSimpleTextureMarkup",
        "GetCoinTextureString",
        "CLASS_SORT_ORDER",
    ] {
        if !lua.globals().get::<Value>(name)?.is_nil() {
            return Err(mlua::Error::runtime(format!(
                "pfUI compatibility cannot replace existing {name}"
            )));
        }
    }
    lua51::install(lua)?;
    encoding::install(lua)?;
    lua.load(include_str!("utilities.lua"))
        .set_name("@manila-pfui/utilities.lua")
        .exec()?;
    let globals = lua.create_table()?;
    lua.load(include_str!("globals.lua"))
        .set_name("@manila-pfui/globals.lua")
        .call::<()>(globals.clone())?;
    lua.set_named_registry_value("manila.pfui.globals_bridge", globals)?;
    lua.load(include_str!("strings.lua"))
        .set_name("@manila-pfui/strings.lua")
        .exec()?;
    lua.load(include_str!("event_registry.lua"))
        .set_name("@manila-pfui/event_registry.lua")
        .exec()?;
    lua.load(include_str!("helpers.lua"))
        .set_name("@manila-pfui/helpers.lua")
        .exec()?;
    lua.load(include_str!("timers.lua"))
        .set_name("@manila-pfui/timers.lua")
        .call::<()>(lua.create_function(|lua, message: String| {
            benilla_ui::script::pfui_host::report_error(lua, message);
            Ok(())
        })?)?;
    widgets::install(lua)?;
    bubbles::install(lua)?;
    benilla_ui::script::pfui_host::install(lua)?;
    icons::install(lua)?;
    loot::install(lua)?;
    maps::Catalog::default().install(lua)?;
    lua.load(include_str!("merchant.lua"))
        .set_name("@manila-pfui/merchant.lua")
        .call::<()>(lua.named_registry_value::<mlua::Table>("manila.pfui.merchant_bridge")?)?;
    lua.load(include_str!("social.lua"))
        .set_name("@manila-pfui/social.lua")
        .call::<()>(lua.named_registry_value::<mlua::Table>("manila.pfui.social_bridge")?)?;
    attributes::install(lua)?;
    nampower::install(lua)?;
    lua.set_named_registry_value("manila.pfui.addon_privates", lua.create_table()?)?;
    lua.load("function UnitClassBase(unit) local _, token = UnitClass(unit); return token end")
        .set_name("@manila-pfui/units.lua")
        .exec()?;
    lua.load(include_str!("enums.lua"))
        .set_name("@manila-pfui/enums.lua")
        .exec()?;
    clock::install(lua)?;
    lua.load(include_str!("items.lua"))
        .set_name("@manila-pfui/items.lua")
        .exec()?;
    lua.load(include_str!("equipment.lua"))
        .set_name("@manila-pfui/equipment.lua")
        .call::<()>(lua.named_registry_value::<mlua::Table>("manila.pfui.equipment_bridge")?)?;
    lua.load(include_str!("tooltips.lua"))
        .set_name("@manila-pfui/tooltips.lua")
        .call::<()>(
            lua.named_registry_value::<mlua::Table>("manila.pfui.equipment_bridge")?
                .get::<mlua::Table>("tooltips")?,
        )?;
    lua.load(include_str!("equipment_actions.lua"))
        .set_name("@manila-pfui/equipment_actions.lua")
        .call::<()>(lua.named_registry_value::<mlua::Table>("manila.pfui.equipment_bridge")?)?;
    lua.set_named_registry_value(INSTALLED, true)
}

/// Execute an addon file with its modern `(addonName, privateTable)` arguments. The caller owns
/// the private table's lifetime and reuses it for every file in that addon, including XML files.
/// Kept here so the stock loader's 1.12 calling convention needs no change.
pub fn run_addon_chunk(
    lua: &Lua,
    source: &[u8],
    chunk_name: &str,
    addon_name: &str,
    private_table: mlua::Table,
) -> mlua::Result<()> {
    lua.load(benilla_ui::source::chunk(source))
        .set_name(chunk_name)
        .set_mode(mlua::ChunkMode::Text)
        .call((addon_name, private_table))
}

/// Provide the class-name table pfUI's environment requires, using the host's localized DBC
/// names. No hardcoded locale/class names are shipped. Install after the utility subset and
/// before loading pfUI. The host calls this again when constructing a replacement VM.
pub fn install_class_names<'a>(
    script: &UiScript,
    names: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> mlua::Result<()> {
    let lua = script.lua();
    if !lua
        .globals()
        .get::<Value>("LOCALIZED_CLASS_NAMES_MALE")?
        .is_nil()
    {
        return Err(mlua::Error::runtime("class-name table already installed"));
    }
    let table = lua.create_table()?;
    for (token, name) in names {
        table.set(token, name)?;
    }
    lua.globals().set("LOCALIZED_CLASS_NAMES_MALE", table)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod aura_tests;

#[cfg(test)]
mod globals_tests;

#[cfg(test)]
mod state_events_tests;

#[cfg(test)]
mod focus_retention_tests;

#[cfg(test)]
mod item_use_tests;
