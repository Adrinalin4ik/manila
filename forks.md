# The forks this repository tracks

`manila` is a fork of a fork. Four other repositories feed it, each for a different reason, and
each is a plain git remote here — nothing is vendored, copied or submoduled. This file says which
is which, what we take from it, and how to reproduce the set on a fresh clone.

The lineage, shortest first:

```
samwhosung/benilla          the client itself, 1.12.1 in Rust + Bevy
  └── Arnesen/wenilla       + the browser (wasm) build and a realm service
        └── Adrinalin4ik/manila   ← this repository
pkuzic/benilla-everwood_graphics   a sibling of benilla: lighting, shadows, water, sky
jhinzuo2/benilla-twow              a sibling of benilla: Turtle WoW client, Android/iOS
```

## The remotes

| remote | repository | what it is | do we merge it? |
|---|---|---|---|
| `manila` | `git@github.com:Adrinalin4ik/manila.git` | **ours.** `main` here is what we build and run. | it is the destination |
| `upstream` | `https://github.com/samwhosung/benilla.git` | the client. Formats, world, UI, protocol, app. | **yes, regularly** |
| `everwood` | `https://github.com/pkuzic/benilla-everwood_graphics.git` | a graphics fork of benilla: dynamic light and shadows, volumetric fog and shafts, SSAO, bloom, colour grading, zone skyboxes, water tiers, foliage wind. ~53 CVars behind two preset ladders. | **yes, since 2026-10-08** |
| `origin` | `git@github.com:Arnesen/wenilla.git` | our direct parent: the browser build, the page side (`web/`), `wenilla-host`, `wenilla-realm`. Most of what it is, we already carry. | occasionally, by cherry-pick |
| `twow` | `https://github.com/jhinzuo2/benilla-twow.git` | a Turtle WoW client built on benilla, with Android and iOS work. | **no** — tracked only |

To recreate them:

```bash
git remote add upstream https://github.com/samwhosung/benilla.git
git remote add everwood https://github.com/pkuzic/benilla-everwood_graphics.git
git remote add origin   git@github.com:Arnesen/wenilla.git
git remote add twow     https://github.com/jhinzuo2/benilla-twow.git
git fetch --all
```

## What we actually take from each

**`upstream` — everything.** benilla is the client; our work rides on it. A sync is a real merge
commit with `upstream/main` as its second parent, never a content copy, because a copy loses the
ancestry and re-raises every conflict at the next sync. Upstream's own rules
(`docs/METHOD.md`, `docs/CONTRIBUTING.md`) govern its code.

Upstream does not build the wasm target and has no CI, so **every sync needs
`cargo check --target wasm32-unknown-unknown` of our own**. The recurring break is `std::time`:
`Instant::now()` compiles on wasm32 and panics when called, so it passes a build and takes the
browser down at runtime. Two separate merges have landed one.

**`everwood` — the graphics system, with the browser booting at the bottom rung.** Merged whole.
Its preset ladder has a `Classic` rung its own table documents as "every lane off, `farclip` at
its registered 350" — the client as it rendered before any of it existed. `GRAPHICS_DEFAULT` in
`cvars/mod.rs` is `#[cfg]`-split so that is what a browser boots into, because its
`seed_graphics_preset` writes the default column over every row a player's config does not carry,
and nothing has measured that this target can afford `High`. A player opts in with
`/console graphicsQuality High`.

**`origin` — by cherry-pick, not by merge.** We are ahead of it on everything that matters to us
and it is ahead of us on the realm service, which we do not run. Its last three commits were read
in full on 2026-10-08: two are `wenilla-realm` dungeon presets (~6,600 lines, useful only if you
host a realm) and one is a client fix whose better half we had already written ourselves.

**`twow` — read, never merged.** Kept as a remote because it solves problems we also have
(Android, iOS, a non-standard server), so its approach is worth reading. Nothing from it is in
`main`. The `manila/twow-parity` branch is older work that `main` has since absorbed.

## Where each one stood on 2026-10-10

A snapshot, not a live table — recompute it rather than trusting it:

```bash
for r in upstream/main everwood/main origin/main twow/main; do
  git merge-base --is-ancestor $r main \
    && echo "$r: fully merged" \
    || echo "$r: $(git rev-list --count --no-merges main..$r) commit(s) not in main"
done
```

| remote | head | state |
|---|---|---|
| `upstream/main` | `d0d1b5de` | fully merged |
| `everwood/main` | `f545081e` | fully merged |
| `origin/main` | `def2a7b0` | 3 commits not in main, deliberately |
| `twow/main` | `5d575f80` | 38 commits not in main; shared base `edec5b22` |

## Contributing back

**benilla takes external pull requests** — three outside contributors were merged in the weeks
before 2026-10-03 — but its bar is 1.12.1 fidelity, not openness. `docs/CONTRIBUTING.md` excludes
"features 1.12.1 does not have, including what 1.12 client mods add (a new key binding, a spell
queue…)" and says a pull request is not the place to propose one. That is why Press and Hold
Casting stays a carry here rather than going upstream.

`gh pr create` needs `-R <owner>/<repo>` spelled out; without it `gh` targets the fork's parent.
