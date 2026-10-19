//! wenilla carry: `GetPlayerShardInfo()` for the shard selector (`assets/ui/ShardSelector.xml`):
//! `enabled, inArea, shard, count` - Player Shards on, whether we stand in a crowd's shard area,
//! the shard of that area we see (1-based) and how many it has. Ours, no reference global: the
//! shards are a wenilla feature (`benilla-app` `player_distance::PlayerShards`), which pushes
//! these every frame. Kept in this module's own cells rather than on `Model`, so the carry
//! touches the upstream script module by two lines only.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use mlua::Lua;

static ENABLED: AtomicBool = AtomicBool::new(false);
static IN_AREA: AtomicBool = AtomicBool::new(false);
/// 0-based; the global answers 1-based, as a Lua index does.
static SHARD: AtomicU32 = AtomicU32::new(0);
static COUNT: AtomicU32 = AtomicU32::new(0);

impl super::UiScript {
    /// Push the shard state: `area` is `(shard, count)` of the area we stand in, `None` outside one.
    pub fn set_player_shard(&mut self, enabled: bool, area: Option<(u32, u32)>) {
        ENABLED.store(enabled, Relaxed);
        IN_AREA.store(area.is_some(), Relaxed);
        let (shard, count) = area.unwrap_or((0, 0));
        SHARD.store(shard, Relaxed);
        COUNT.store(count, Relaxed);
    }
}

/// Register `GetPlayerShardInfo`.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "GetPlayerShardInfo",
        lua.create_function(|_, ()| {
            Ok((
                ENABLED.load(Relaxed),
                IN_AREA.load(Relaxed),
                i64::from(SHARD.load(Relaxed)) + 1,
                i64::from(COUNT.load(Relaxed)),
            ))
        })?,
    )
}
