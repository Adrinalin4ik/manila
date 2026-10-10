//! Real chat-bubble frames driven by the host's spawn, fade, projection and lifetime verdicts.

use mlua::{Function, Lua};

/// One currently projected bubble, in far-to-near stacking order. Coordinates are screen pixels.
pub struct Bubble<'a> {
    pub guid: u64,
    pub born: f32,
    pub text: &'a str,
    pub color: [f32; 3],
    pub alpha: f32,
    pub visible: bool,
    pub x: f32,
    pub bottom: f32,
    pub wrap: f32,
    pub margin: f32,
    pub border: f32,
    pub font_height: f32,
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let sync: Function = lua
        .load(include_str!("bubbles.lua"))
        .set_name("@manila-pfui/bubbles.lua")
        .eval()?;
    lua.set_named_registry_value("manila.pfui.bubbles", sync)
}

pub fn sync<'a>(lua: &Lua, bubbles: impl IntoIterator<Item = Bubble<'a>>) -> mlua::Result<()> {
    let sync: Option<Function> = lua.named_registry_value("manila.pfui.bubbles")?;
    let Some(sync) = sync else {
        return Ok(());
    };
    let rows = lua.create_table()?;
    for (index, bubble) in bubbles.into_iter().enumerate() {
        let row = lua.create_table()?;
        row.set("guid", format!("0x{:016X}", bubble.guid))?;
        row.set("born", bubble.born)?;
        row.set("text", bubble.text)?;
        row.set("r", bubble.color[0])?;
        row.set("g", bubble.color[1])?;
        row.set("b", bubble.color[2])?;
        row.set("alpha", bubble.alpha)?;
        row.set("visible", bubble.visible)?;
        row.set("x", bubble.x)?;
        row.set("bottom", bubble.bottom)?;
        row.set("wrap", bubble.wrap)?;
        row.set("margin", bubble.margin)?;
        row.set("border", bubble.border)?;
        row.set("fontHeight", bubble.font_height)?;
        rows.set(index + 1, row)?;
    }
    sync.call(rows)
}
