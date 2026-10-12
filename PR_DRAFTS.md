# Pull request drafts

GitHub pull requests cannot be opened over SSH, so each branch below is pushed to
`Thorsten42/manila` and has a link that opens the pull request form with the right base. Paste the
title and text.

Every draft ends with the same disclaimer: the work is AI-generated and was verified locally.

benilla asks for one change per pull request (`docs/CONTRIBUTING.md`), so its fixes are five
small branches, each one commit on `samwhosung/benilla` `main` at `0127b869`.

Verified on that base, Linux, no install (`WOW_DATA=`): `benilla-ui` 2,210 tests pass,
`benilla-world` doodad tests pass, `benilla-app` has one failure,
`schedule_tests::the_schedules_have_no_more_undeclared_orders_than_the_ceilings_say`, which fails
on pristine `main` too. `cargo clippy -p benilla-ui -p benilla-world -p benilla-app -- -D warnings`
is clean and the touched files pass `rustfmt --check`. `scripts/gates.sh` was not run as a whole.

---

## benilla 1: nameplate name

https://github.com/samwhosung/benilla/compare/main...Thorsten42:manila:benilla/nameplate-unknownobject?expand=1

**Title:** Name a plate UNKNOWNOBJECT until its unit's name answers

**What 1.12.1 does:** a unit whose name query has not answered is named `UNKNOWNOBJECT`; its
tooltip and `UnitName` already do this here (`0x517216`).

**What benilla did:** the nameplate feed passed an empty name, so the plate's name region read
`nil`.

**How to see it:** stand where new creatures stream in with nameplates on and read a plate's name
region the frame it appears. A nameplate addon that keys a table on the name raises
"table index is nil".

**Change:** `benilla-app/src/vplates.rs` uses the `UNKNOWNOBJECT` global until the name resolves.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.

---

## benilla 2: quest log event objective

https://github.com/samwhosung/benilla/compare/main...Thorsten42:manila:benilla/quest-event-objective?expand=1

**Title:** Build the quest log's event objective line

**What 1.12.1 does:** `GetQuestLogLeaderBoard` gives a quest's end text as its first line when it
is non-empty (`0x4e0278`, index 1 at `0x4e0285`), typed `"event"` (`0x4e0293`), done by the
slot's COMPLETE bit (`0x4e02a2`). Read from build 5875 with objdump.

**What benilla did:** `build_objectives` documented the event line as not built. A quest whose
only objective is an event, such as the druid quest "Body and Heart" (6001), had no lines.

**How to see it:** take such a quest and call `GetNumQuestLeaderBoards`: 0 before, 1 after.
Quest addons that count lines showed the quest as complete.

**Change:** the end text is pushed as the first line. It carries no count, so the progress toast
does not read it as a line that advanced. Test included.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.

---

## benilla 3: font inheritance

https://github.com/samwhosung/benilla/compare/main...Thorsten42:manila:benilla/font-inherits-link?expand=1

**Title:** Keep a declared font linked to the font it inherits

**What 1.12.1 does:** a `<Font inherits=…>` stays linked to its parent (`LoadXML 0x783c30` calls
the `SetFontObject` link `0x770c60` for `inherits=`, `0x783ce6`), so a setter on the parent
reaches it.

**What benilla did:** the chain was flattened at load; `font.rs` listed this as not built.

**How to see it:** `GameFontNormal:SetFont("Fonts\\ARIALN.TTF", 14)` then
`GameFontHighlight:GetFont()`: the old face before, the new one after. An addon that restyles the
client this way left every highlighted button label in the stock face.

**Change:** each declared font records its parent and what it holds itself; a parent's setter
re-derives the chain and repaints the regions. A property set on a child directly is no longer
supplied by its parent. Test included.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.

---

## benilla 4: tooltip size inside Show

https://github.com/samwhosung/benilla/compare/main...Thorsten42:manila:benilla/tooltip-size-in-show?expand=1

**Title:** Size a tooltip in the call that shows it

**What 1.12.1 does:** the tooltip is sized inside its show; `GetWidth` is right in the same Lua
call.

**What benilla did:** the lines were measured on the next layout pass, so `GetWidth` read 0 and
`GetLeft`/`GetRight` had no rect straight after `Show`.

**How to see it:** `tt:SetOwner(f, "ANCHOR_RIGHT"); tt:AddLine("Tough Jerky"); tt:Show();
print(tt:GetWidth())`: 0 before, the measured width after. Addons that anchor a second tooltip
beside the first in the same script overlapped them.

**Change:** `show_override` measures the plate's lines. A no-op without a host font engine. Test
included. Reference evidence here is observed behaviour, not a traced address.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.

---

## benilla 5: transport props

https://github.com/samwhosung/benilla/compare/main...Thorsten42:manila:benilla/transport-prop-anim?expand=1

**Title:** Keep a transport's animated props following the hull

**What 1.12.1 does:** a ship's sail moves with the ship.

**What benilla did:** an animated doodad is parked while its draw sphere is out of view, and a
parked rig's palette rows and origin are not rewritten. The sphere is a world position fixed at
spawn; for a prop of a transport that is where the ship was, so the sail was parked on a stale
verdict and stayed behind in the world until the old spot came back into view.

**How to see it:** ride a boat and watch the sail from the deck.

**Change:** a prop spawned under a WMO GameObject is marked as carried, and a carried host is
judged by its parts' visibility alone. No test: it needs a moving transport. Seen on a Turtle
core; not re-checked on vmangos.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.

---

## manila: Turtle ships and the test suite

https://github.com/Adrinalin4ik/manila/compare/main...Thorsten42:manila:fix/turtle-ships-and-test-repairs?expand=1

**Title:** Fix ships on a Turtle core and get the test suite compiling again

Nine commits on `main` at `7e71f725`. One user-visible fix, the rest make the tests build and
pass again.

**Ships and zeppelins never appeared on a Turtle core.** That core sends every MO transport at
login with its path progress and no pose, and numbers them 1..14 in the guid where vmangos passes
the template entry as the counter. A create with no pose was dropped in `decode_objects`, and the
template was asked for by the guid's counter. A transport is now created at the origin (its
timetable places it from the first tick) and its template follows `OBJECT_FIELD_ENTRY`, which is
right on both cores. Seen live at Rut'theran on TortoiseWoW 1.18.1.

**Press-and-hold casting after a UI reload** kept a binding's action and eligibility from the old
VM; the derived values now reset with the session.

**Test suite.** On `main` the app tests do not compile and one UI test fails:

- `options_tests.rs` has an unclosed tuple after the Water Reflections row was removed.
- The CVar default test's table declares 41 rows and holds 42 since the Lamp Fog change.
- App fixtures needed the current Bevy and async APIs (login, realm and character parks, CVar
  observers, skin composite).
- The camera zoom test read the old queue.
- `GetDebugStats` assertions split from the platform-query ones.
- Unit snapshots went stale without the optional GUID index (the Cat Form creature-type test).
- The chat sweep counter is now keyed on the VM session (the session-memo structural test).

With these the app suite compiles; 4,377 tests pass and 10 fail on Linux with no install. Two of
those ten come from the Lamp Fog commits and are not fixed here
(`cvars::tests::help_strings_have_no_broken_continuations`,
`options_tests::lamp_fog_dropdown_is_localised_and_live`); the other eight are older.

Not in this pull request: the client fixes that belong to benilla (nameplate name, quest event
objective, font inheritance, tooltip size, transport props). They are offered to
`samwhosung/benilla` separately and will arrive with an upstream merge.

**Disclaimer:** this change and its description were written by an AI assistant (Claude), directed by me. It was verified locally on Linux: it builds, the test suites named above were run, and the behaviour was checked against a local server where the text says so. Please review it as you would any outside contribution.
