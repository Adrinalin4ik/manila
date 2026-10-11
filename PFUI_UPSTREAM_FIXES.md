# Client fixes found while adding pfUI support

Branch `feat/pfui-compatibility` on `Thorsten42/manila`, rebased on `Adrinalin4ik/manila`
`main` at `7e71f725`. Each fix below is its own commit and touches no pfUI code, so it can be
cherry-picked alone. The pfUI extension itself (`crates/manila-pfui`, the `pfui_host` files and
the `#[cfg(feature = "pfui")]` hooks) is not listed here.

The split is by where the fault shows:

- **benilla** means the shared client disagrees with the 1.12 reference client on any server.
- **manila** means the fault is in fork-only code, or only shows against a Turtle core.

Where a reference address is given, it was read from build 5875 `WoW.exe` with `objdump`.
Nothing was copied from it.

## benilla: the client disagrees with the 1.12 reference

| # | Commit | Fault | Fix | Files |
|---|--------|-------|-----|-------|
| 1 | `ff25c682` | `Frame:GetWidth()` and `GetHeight()` divided the resolved span by the effective scale and preferred it over an authored size. The reference (`0x7a1e00`, `0x7a2030`) returns the authored size unless it is exactly 0, else the resolved span with the scale still in it. Addons written for 1.12 divide the scale out themselves, so under any UI scale other than 1 their bars overfill and an empty bar resolves inside out. | Return the authored size when non-zero, else the resolved span undivided. | `benilla-ui/src/script/object/layout_methods.rs` |
| 2 | `94d672e6` | `GameTooltip:SetOwner` left a shown tooltip shown. The reference binding hides it first (`0x531179` clears `+0xd0`, `0x531183` calls the hide slot `vtbl+0x84`), so the next show is a fresh one and `OnHide`/`OnShow` fire on the tooltip and its children. Addons that position the tooltip in a child's `OnShow` were skipped when the mouse moved from one unit straight to the next. | Hide the tooltip at the top of the `SetOwner` verb. | `benilla-ui/src/script/tooltip/verbs.rs` |
| 3 | `6f02cfb1` | A `<Font inherits=…>` chain was flattened at load, so a setter on a parent font never reached the fonts declared from it. The module doc listed this as not built. The reference links them live (`LoadXML 0x783c30`). `GameFontNormal:SetFont(...)` did not reach `GameFontHighlight`, so a highlighted button label switched face. | Each declared font records its parent and what it holds itself; a parent's setter re-derives the chain and repaints the regions. | `benilla-ui/src/script/font.rs`, `script/model.rs`, `loader/mod.rs` |
| 4 | `f953ea37` | The quest log's `event` objective line was not built (noted as such in `build_objectives`). The reference gives a quest's end text as leaderboard line 1, typed `"event"`, done by the slot's COMPLETE bit (`0x4e0278`..`0x4e02a2`). A quest whose only objective is an event had no lines, and quest addons read it as complete. | Push the end text as the first line when it is non-empty. It carries no count, so the progress toast ignores it. | `benilla-app/src/ui_quest_log.rs` |
| 5 | `50452d3f` | A tooltip had no size in the Lua call that showed it: `GetWidth()` read 0 until the next layout pass, so a second tooltip anchored to it in the same script overlapped it. The reference measures inline. | Measure the tooltip's lines in `Show`, `SetText` and the empty-plate path. | `benilla-ui/src/script/tooltip/mod.rs`, `tooltip/verbs.rs` |
| 6 | `1227c4c8` | A nameplate for a unit whose name query had not answered carried an empty name, so its name region read `nil`. The unit's tooltip and `UnitName` use `UNKNOWNOBJECT` in that state. Nameplate addons that key a table on the name raised "table index is nil". | Use `UNKNOWNOBJECT` until the name answers. | `benilla-app/src/vplates.rs` |
| 7 | `ed41ad2d` | Unit snapshots went stale when the optional GUID index was absent: after Cat Form the creature type stayed Humanoid. | Fall back to the descriptor change query. | `benilla-app/src/ui_unit.rs` |
| 8 | `e19f1935` | Player aura expiration times shifted with the frame delta, because the aura feed runs before the Lua tick and joined a current duration stamp to the previous frame's Lua time. | Map the frame through the existing `UiClock` pair before joining the stamp. | `benilla-app/src/ui_aura.rs`, new `ui_aura/clock.rs` |
| 9 | `4a75490e` | The chat message-sweep counter is cumulative per VM, but the remembered value survived a UI reload, which suppressed journal accounting until the new VM caught up. | Key the remembered value on the VM session. | `benilla-app/src/ui_script/extract/mod.rs` |
| 15 | `886e7f80` | A transport's animated props (a ship's sail) stayed behind in the world while the hull moved on. An animated doodad is parked while its draw sphere is out of view, and a parked rig's palette rows and origin are not rewritten; the sphere is a world position fixed at spawn, which for a prop of a transport is where the ship was. | Mark a prop spawned under a WMO GameObject as carried and judge a carried host by its parts' visibility alone. | `benilla-world/src/doodad_anim/mod.rs`, `benilla-app/src/entities/wmo_props.rs` |
| 16 | `f9d10fa7` | Goes with fix 1: the options page set its body width from the scroll frame's `GetWidth`, which relied on the old answer. Under a UI scale every row label was cut short. | Take the width from the frame's edges, which are in its own units on either behaviour. | `benilla-app/assets/ui/OptionsFrame.xml` |

Tests that fail or do not compile without these, with no behaviour change:

| Commit | What |
|--------|------|
| `19ea8f45` | The camera zoom regression test reads the shared binding input queue, where the zoom verbs now publish. |
| `dbf43770` | App test fixtures brought up to the current Bevy and async APIs (login, realm and character parks, CVar observers, skin composite). Tests only. |

## manila: fork-only code, or only against a Turtle core

| # | Commit | Fault | Fix | Files |
|---|--------|-------|-----|-------|
| 10 | `94eecaf1` | No ship or zeppelin ever appeared on a Turtle core. That core sends every MO transport at login with its path progress and no pose, and numbers them 1..14 in the guid where vmangos passes the template entry as the counter. A create with no pose was dropped in `decode_objects`, and the template was asked for by the guid's counter, which names an unrelated object. | Create a transport without a pose at the origin (its timetable places it from the first tick), and look its template up by `OBJECT_FIELD_ENTRY`, which is right on both cores. | `benilla-protocol/src/events/decode.rs`, `benilla-app/src/go_templates.rs`, `net/objects.rs` |
| 11 | `4f92473d` | Press-and-hold casting kept a binding's action, eligibility and release suppression across a UI reload, so a changed binding body inherited the old body's state. | Reset the derived values with the VM session and keep the physical key latch. | `benilla-app/src/bindings/hold_cast.rs` |
| 12 | `6d9ef867` | The options test fixture had an unclosed tuple after the Water Reflections row was removed, so the app tests did not compile and `rustfmt` could not parse the file. | Restore the delimiter. | `benilla-app/src/ui_script/options_tests.rs` |
| 13 | `e0808fed` | The `GetDebugStats` test mixed the PC platform-query assertions with the fork's own diagnostic string contract. | Test the fork's string separately. Test only. | `benilla-ui/src/script/client.rs` |
| 14 | `205bb4ff` | After the Lamp Fog change the fixed-size table in the CVar default test declares 41 rows and holds 42, so the app tests do not compile on `main` at `7e71f725`. | Declare 42, which the assertion below it already reads. Test only. | `benilla-app/src/cvars/mod.rs` |

## Changes to shared types that pfUI support needs

These are not bug fixes, but they change upstream files outside a `cfg` gate, so a maintainer
may prefer to take them rather than carry a conflict.

| Commit | Change | Files |
|--------|--------|-------|
| `7c8b40cf` | `QuadContent::ModelPane` carries the pane's `SetUnit` token (`unit: Option<String>`). Without the pfUI feature it is unused. | `benilla-ui/src/script/types.rs`, `script/extract.rs`, `benilla-app/src/ui_script/extract/mod.rs` |
| `e5711f3a` | A round portrait slot can be spawned looping Stand instead of frozen, and re-bakes when that changes. Without the pfUI feature it is always frozen, as before. | `benilla-app/src/portrait/mod.rs` |
| `f4019171` | The vendored Lua accepts the original 5.1 vararg, length and modulo syntax for a VM that sets a private registry key. Stock VMs keep the stock grammar. | `third_party/lua-src/lua-5.1.5/lparser.c`, new `lmanila.h` |

## Known and not fixed

- **Two app tests fail since the Lamp Fog commits** (`28beeb08`, `4e15658d`), once the suite
  compiles: `cvars::tests::help_strings_have_no_broken_continuations` (the `lampFog` help text
  has a line break without a continuation) and
  `ui_script::options_tests::lamp_fog_dropdown_is_localised_and_live`. Not fixed here.
- **Eight more app tests fail on Linux with no install (`WOW_DATA=`)**, before and after this
  branch:
  `dynamic_interior::…water_and_lava_bridge…`, `entities::attach::redress::…a_gear_change_restamps…`,
  `game_plugins::schedule_tests::…no_more_undeclared_orders_than_the_ceilings_say`, three in
  `game_tip::tests`,
  `perf::journal::…the_header_ends_with_the_gpu_cells_in_bucket_order` and
  `ui_script::shape_gate::every_query_binding_answers_the_reference_s_return_arity`.
- **The game menu stacks its rungs flush**, where the stock `GameMenuFrame.xml` leaves one unit
  between two (`GameMenuAdapters.xml`). An addon that draws a border one unit outside each
  button then covers a neighbour's border. The pfUI extension puts the gap back for itself;
  the adapter still stacks flush.
- **A ScrollingMessageFrame row is a whole font height** (`rows: u16`), so a line that needs
  more height can only take whole extra rows.
- **`SetScript` rejects `OnTooltipSetItem`**, which the Turtle build of AtlasLoot sets; that
  file then stops loading.

## How to take a fix

```bash
git remote add thorsten git@github.com:Thorsten42/manila.git
git fetch thorsten feat/pfui-compatibility
git cherry-pick ff25c682      # any single commit from the tables above
```

Fixes 1 to 5, 8, 10 and 11 come with a test in the same commit.
