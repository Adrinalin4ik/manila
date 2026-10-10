/* Optional Lua 5.1 expressions for an extended client; an unset registry key
 * preserves benilla's reference Lua 5.0 grammar. No addon-visible switch. */
#ifndef lmanila_h
#define lmanila_h

static int manila_lua51 (lua_State *L) {
  int enabled;
  lua_pushliteral(L, "manila.pfui.lua51");
  lua_rawget(L, LUA_REGISTRYINDEX);
  enabled = lua_toboolean(L, -1);
  lua_pop(L, 1);
  return enabled;
}

#endif
