//! A character body's appearance inputs: its look ([`CharLook`]), its worn display ids
//! ([`WornEquip`]) and its per-appearance materials ([`build_char_skin_materials`]).

use benilla_formats::{CharSkinSlot, ModelBlend};
use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::net::{NetEntity, ObjectStore};
use benilla_assets::materials::WowModelMaterial;
use benilla_assets::{repeat_texture_authored, LockRecover, WorldAssets};

use super::super::{DisplayModel, EntityPart, SkinKey, SkinSections};

/// A character-model body's appearance, from the wire or from its display's
/// CreatureDisplayInfoExtra row; it drives both the geoset selection and the skin materials.
pub(super) struct CharLook {
    pub(super) race: u8,
    pub(super) sex: u8,
    /// skinColor: keys the composite and the extra-skin BLP (tauren fur, M2 type 8), which a baked
    /// NPC atlas does not cover.
    pub(super) skin: u8,
    pub(super) hair_style: u8,
    pub(super) hair_color: u8,
    pub(super) facial_hair: u8,
    pub(super) body: BodySkin,
}

/// The body-skin atlas source for a [`CharLook`].
pub(super) enum BodySkin {
    /// Composited live from CharSections, cached per appearance: a player, or an NPC row with no
    /// bake name.
    Composite { face: u8 },
    /// A character-model NPC's shipped atlas under `Textures\BakedNpcTextures\`
    /// (CreatureDisplayInfoExtra field 18), loaded as is, never re-baked.
    Baked(String),
}

/// A net entity's character look; `None` unless its display is a character-model body.
pub(super) fn resolve_char_look(
    net: &NetEntity,
    dm: Option<&DisplayModel>,
    entity: Entity,
    stores: &Query<&ObjectStore>,
) -> Option<CharLook> {
    // The look follows the display, not the entity kind: the display's own appearance row first,
    // the wire's only for a character body without one, as the reference's race and sex getters
    // answer (`0x60c690`). A druid form is a plain creature model and has no look.
    let d = dm?;
    if let Some(npc) = d.npc_appearance.as_ref() {
        return Some(CharLook {
            race: npc.race,
            sex: npc.sex,
            skin: npc.skin,
            hair_style: npc.hair_style,
            hair_color: npc.hair_color,
            facial_hair: npc.facial_hair,
            body: match &npc.bake_name {
                Some(name) => BodySkin::Baked(name.clone()),
                None => BodySkin::Composite { face: npc.face },
            },
        });
    }
    // A corpse's look is its own `CORPSE_FIELD_BYTES_1/_2` snapshot from death, never the
    // owner's `PLAYER_BYTES` (`0x5d6260`); a bone pile has none, `0x5d6291`'s early skip.
    if net.kind == EntityKind::Corpse && d.is_character_body {
        let look = super::super::corpse::corpse_char_look(stores.get(entity).ok())?;
        return Some(CharLook {
            race: look.race,
            sex: look.sex,
            skin: look.skin,
            hair_style: look.hair_style,
            hair_color: look.hair_color,
            facial_hair: look.facial_hair,
            body: BodySkin::Composite { face: look.face },
        });
    }
    if net.kind == EntityKind::Player && d.is_character_body {
        // Race and sex from `UNIT_FIELD_BYTES_0`. vmangos leaves an all-zero field out of the
        // create mask (`Object.cpp:1149`), and an absent field is 0, so each customization byte
        // defaults to 0.
        let s = &stores.get(entity).ok()?.0;
        return Some(CharLook {
            race: s.unit_race()?,
            sex: s.unit_gender()?,
            skin: s.player_skin().unwrap_or(0),
            hair_style: s.player_hair_style().unwrap_or(0),
            hair_color: s.player_hair_color().unwrap_or(0),
            facial_hair: s.player_facial_hair().unwrap_or(0),
            body: BodySkin::Composite {
                face: s.player_face().unwrap_or(0),
            },
        });
    }
    None
}

/// The `mpq://` URL of a baked NPC atlas under `Textures\BakedNpcTextures\`.
fn baked_npc_url(bake_name: &str) -> String {
    format!(
        "mpq://textures/bakednpctextures/{}",
        bake_name.replace('\\', "/").to_ascii_lowercase()
    )
}

/// The worn display ids behind a character body's geoset selection and, for a player, its region
/// composite; all zero, the naked body, for anything without a look.
#[derive(Default)]
pub(super) struct WornEquip {
    /// Shirt, chest, belt, pants, boots, wrist, gloves, tabard (bodyslots 2 to 9).
    pub(super) bodyslots: [u32; 8],
    pub(super) cloak: u32,
    pub(super) helm: u32,
    /// The wearer's guild emblem, a player's only: CreatureDisplayInfoExtra has no guild column,
    /// so an NPC's tabard keeps its own art.
    pub(super) emblem: Option<benilla_formats::GuildEmblem>,
    pub(super) tabard_preview: bool,
}

pub(super) fn resolve_worn_equip(
    net: &NetEntity,
    equipment: Option<&super::super::Equipment>,
    dm: Option<&DisplayModel>,
) -> WornEquip {
    match net.kind {
        // A corpse's `Equipment` comes from its 19 `CORPSE_FIELD_ITEM` slots, already
        // ItemDisplayInfo ids.
        EntityKind::Player | EntityKind::Corpse => equipment
            .map(|e| WornEquip {
                bodyslots: e.bodyslots,
                cloak: e.cloak,
                helm: e.helm,
                emblem: e.emblem,
                tabard_preview: e.tabard_preview,
            })
            .unwrap_or_default(),
        // An NPC wears its CreatureDisplayInfoExtra columns, ItemDisplayInfo ids by bodyslot: 2 to
        // 9 are the armor slots, 0 the helm, and there is no cloak column.
        EntityKind::Unit => dm
            .and_then(|d| d.npc_appearance.as_ref())
            .map(|npc| WornEquip {
                bodyslots: std::array::from_fn(|i| npc.equipment[i + 2]),
                cloak: 0,
                helm: npc.equipment[0],
                emblem: None,
                tabard_preview: false,
            })
            .unwrap_or_default(),
        _ => WornEquip::default(),
    }
}

/// The worn geoset selectors for a set of display ids, the inputs of `0x477520`'s branches B1 to
/// B8: each row's geoset columns, the cloak group and the helm's hide-mask rows (`0x4799a0`).
/// `tabard_preview` (B6) is set only while the tabard designer is open, as in the reference.
pub(in crate::entities) fn equip_geosets(
    displays: Option<&super::super::ItemDisplays>,
    bodyslots: &[u32; 8],
    cloak: u32,
    helm: u32,
    tabard_preview: bool,
) -> benilla_formats::EquipGeosets {
    let mut eg = benilla_formats::EquipGeosets {
        tabard_preview,
        ..Default::default()
    };
    if let Some(d) = displays {
        let mut worn: [Option<&benilla_formats::ItemDisplay>; 8] = [None; 8];
        for (i, id) in bodyslots.iter().enumerate() {
            if *id != 0 {
                worn[i] = d.catalog.get(*id);
                eg.bodyslots[i] = worn[i].map(|row| row.geoset_groups);
            }
        }
        // B3's gate is the ArmLower tile's occupancy in the composite plan, not a worn chest.
        eg.forearm_dressed = benilla_formats::forearm_dressed(&worn);
        if cloak != 0 {
            eg.cloak = d.catalog.get(cloak).map(|row| row.geoset_groups[0]);
        }
        if helm != 0 {
            // Only a display naming a head model hides hair, facial hair and ears: NPC head columns
            // can name model-less rows that still carry a full hide mask.
            eg.helm_vis = d
                .catalog
                .get(helm)
                .and_then(benilla_formats::ItemDisplay::worn_helm_vis);
        }
    }
    eg
}

/// One character slot's material set: (steady, interior-matte, fade blend, interior-bake,
/// interior-bake blend, depth-prime twin), shared by every body of one look.
pub(super) type MatQuint = (
    Handle<WowModelMaterial>,
    Handle<WowModelMaterial>,
    Handle<WowModelMaterial>,
    Handle<WowModelMaterial>,
    Handle<WowModelMaterial>,
    Option<Handle<WowModelMaterial>>,
);

/// `(body, hair, object, skin_extra)`, body and extra skin as (single-sided, two-sided) pairs; a
/// slot is `None` for an absent row (a bald style, a non-fur race) or missing tables.
pub(super) type CharSkinMaterials = (
    Option<(MatQuint, MatQuint)>,
    Option<MatQuint>,
    Option<MatQuint>,
    (Option<MatQuint>, Option<MatQuint>),
);

/// The `WOW_PROBE_SHARED_SKIN` pricing lever.
fn shared_skin_probe() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_PROBE_SHARED_SKIN").is_some())
}

/// Build a character body's per-appearance materials: the body atlas (composited live, or an NPC's
/// baked BLP), the hair, the cape and the extra skin, each at its batches' own blend and
/// sidedness from `parts`. Without the tables or the light buffer, parts keep their built material.
#[allow(clippy::type_complexity)]
pub(super) fn build_char_skin_materials(
    look: &CharLook,
    // The armor ids paint only a live composite; a baked NPC atlas already carries its gear.
    equip: [u32; 8],
    cloak: u32,
    // Painted over a tabard whose display asks for it; `None` leaves the tabard's own art.
    emblem: Option<benilla_formats::GuildEmblem>,
    tabard_preview: bool,
    displays: Option<&super::super::ItemDisplays>,
    sections: Option<&SkinSections>,
    world_assets: Option<&WorldAssets>,
    parts: &[EntityPart],
    images: &mut Assets<Image>,
    skin_cache: &mut benilla_assets::SpatialCache<SkinKey, Handle<Image>>,
    asset_server: &AssetServer,
    mats: &mut benilla_world::model_render::M2BatchMaterials,
    // `Some` on the world's own dressing paths, where a body may wear its base skin for a few
    // frames while the Worker composites; `None` on the character-select preview, which is one
    // body on a still screen and must be right the first time it is drawn.
    mut pending: Option<Mut<super::skin_worker::PendingSkins>>,
) -> CharSkinMaterials {
    let (Some(sections), true) = (sections, mats.ready()) else {
        return (None, None, None, (None, None));
    };
    // The engine's variant set in `MatQuint` order.
    let quint = |v: benilla_world::model_render::BatchVariants| {
        (
            v.steady,
            v.interior,
            v.fade_blend,
            v.interior_bake,
            v.interior_bake_blend,
            v.zfill,
        )
    };

    // The composite reads its BLPs synchronously off the shared chain, once per look behind the
    // cache.
    let body_tex: Option<Handle<Image>> = match &look.body {
        BodySkin::Baked(name) => Some(asset_server.load::<Image>(baked_npc_url(name))),
        BodySkin::Composite { face } => world_assets.and_then(|world| {
            let mut key = SkinKey {
                race: look.race,
                sex: look.sex,
                skin: look.skin,
                face: *face,
                facial_hair: look.facial_hair,
                hair_style: look.hair_style,
                hair_color: look.hair_color,
                equip,
                emblem,
                tabard_preview,
            };
            // `WOW_PROBE_SHARED_SKIN` prices a shared body material and is never a look: one key
            // for every body lets Bevy's batcher instance them.
            if shared_skin_probe() {
                key = SkinKey {
                    race: 1,
                    sex: 0,
                    skin: 0,
                    face: 0,
                    facial_hair: 0,
                    hair_style: 0,
                    hair_color: 0,
                    equip: [0; 8],
                    emblem: None,
                    tabard_preview: false,
                };
            }
            match skin_cache.fetch(&key) {
                Some(handle) => Some(handle),
                None => {
                    // **The composite's own meter** (`skins_new`/`skin_us`). A guard rather than a
                    // pair of statements because this arm leaves through `?` twice below: a look
                    // whose sections or display rows are missing costs the same reads and decode
                    // and then returns nothing, and a frame does not care that the result was
                    // dropped. Drop order runs it on every exit.
                    //
                    // **What it counts changed when the Worker arrived.** A deferred body now
                    // takes TWO samples — this one (plan + base-only render + post) and the
                    // drain's (decode + upload) — so `skins_new` is skin *events*, not bodies,
                    // and it is not comparable with journal 43's 437. `skin_us` is unchanged in
                    // meaning: microseconds of the drawing thread spent on skins, which is the
                    // number this whole change exists to move.
                    struct Meter(bevy::platform::time::Instant);
                    impl Drop for Meter {
                        fn drop(&mut self) {
                            crate::perf::journal::note_skin_composite(
                                self.0.elapsed().as_micros() as u64,
                            );
                        }
                    }
                    let _meter = Meter(bevy::platform::time::Instant::now());
                    // The worn ItemDisplayInfo rows whose region textures dress the atlas
                    // (decision 0074); an unknown/zero display id contributes nothing.
                    let catalog = displays.map(|d| &d.catalog);
                    let mut worn: [Option<&benilla_formats::ItemDisplay>; 8] = [None; 8];
                    if let Some(catalog) = catalog {
                        for (i, id) in equip.iter().enumerate() {
                            if *id != 0 {
                                worn[i] = catalog.get(*id);
                            }
                        }
                    }
                    let plan = sections.0.plan_body(
                        key.race,
                        key.sex,
                        key.skin,
                        key.face,
                        key.facial_hair,
                        key.hair_style,
                        key.hair_color,
                        worn,
                        key.emblem,
                        key.tabard_preview,
                    )?;
                    let chain = &mut world.chain.lock_recover();
                    // **The expensive half leaves, when there is somewhere for it to go.** With a
                    // Worker we render the BASE ONLY here — one decode, no blits — cache that
                    // handle under the real key, and let `skin_worker::drain_skin_worker` replace
                    // the image behind the same handle when the atlas comes back. The body wears
                    // its bare skin for those frames and is never re-dressed; see
                    // `skin_worker`'s header for why a placeholder MATERIAL could not work.
                    //
                    // Without one (native, the character-select preview, a browser that would not
                    // start a Worker) `pending` is `None` or the request is refused, and this is
                    // the same synchronous composite it has always been.
                    let mut base_only = None;
                    if pending.is_some() {
                        base_only = sections
                            .0
                            .render_plan(
                                chain,
                                &benilla_formats::BodyPlan {
                                    base: plan.base.clone(),
                                    steps: Vec::new(),
                                },
                            )
                            .ok()
                            .flatten();
                    }
                    let deferred = base_only.is_some();
                    let composed = match base_only {
                        Some(atlas) => atlas,
                        None => sections.0.render_plan(chain, &plan).ok()??,
                    };
                    // Through the upload gate like every texture: a no-op on this RGBA8
                    // composite, but it keeps the format and the bytes in agreement.
                    let handle = images.add(repeat_texture_authored(
                        benilla_assets::for_upload(composed),
                        (true, true),
                    ));
                    // The request only after the handle exists, since the handle is where the
                    // answer goes. A refusal here leaves the base-only atlas standing, so the
                    // work is redone synchronously rather than left as a bare body for ever.
                    if deferred {
                        let posted = pending
                            .as_deref_mut()
                            .is_some_and(|p| p.request(&handle, plan.clone()));
                        if !posted {
                            if let Ok(Some(atlas)) = sections.0.render_plan(chain, &plan) {
                                images.insert(
                                    handle.id(),
                                    repeat_texture_authored(
                                        benilla_assets::for_upload(atlas),
                                        (true, true),
                                    ),
                                );
                            }
                        }
                    }
                    skin_cache.insert(key, handle.clone());
                    Some(handle)
                }
            }
        }),
    };
    // A single-sided and a two-sided set, chosen per batch by its own M2 `0x04`: the robe skirt
    // (geoset 1302) is authored two-sided, the closed body is not.
    let body = body_tex.map(|tex| {
        (
            quint(
                mats.char_variants(tex.clone(), ModelBlend::Opaque, false)
                    .expect("light buffer checked at entry"),
            ),
            quint(
                mats.char_variants(tex, ModelBlend::Opaque, true)
                    .expect("light buffer checked at entry"),
            ),
        )
    });

    // The hair texture (M2 type 6) also dresses the facial hair of races whose beards are
    // geometry, so it resolves through `hair_mesh_texture`'s bald fallback: a bald orc has a beard.
    let hair = sections
        .0
        .hair_mesh_texture(look.race, look.sex, look.hair_style, look.hair_color)
        .and_then(|path| {
            let hair_part = parts
                .iter()
                .find(|p| p.char_slot == Some(CharSkinSlot::Hair))?;
            let tex = asset_server.load::<Image>(format!(
                "mpq://{}",
                path.replace('\\', "/").to_ascii_lowercase()
            ));
            mats.char_variants(tex, hair_part.blend, hair_part.two_sided)
                .map(&quint)
        });

    // The cape: the cloak's ItemDisplayInfo `model_texture[0]`, a BLP under
    // `Item\ObjectComponents\Cape\`, on the body's type-2 batches.
    let object = (cloak != 0)
        .then_some(())
        .and_then(|()| displays?.catalog.get(cloak)?.model_texture[0].as_deref())
        .and_then(|tex_name| {
            let part = parts
                .iter()
                .find(|p| p.char_slot == Some(CharSkinSlot::Object))?;
            let tex = asset_server.load::<Image>(format!(
                "mpq://item/objectcomponents/cape/{}.blp",
                tex_name.to_ascii_lowercase()
            ));
            mats.char_variants(tex, part.blend, part.two_sided)
                .map(&quint)
        });

    // The extra skin (tauren fur, M2 type 8): CharSections section 0 `TextureName[1]` by
    // skinColor, loaded plain as the reference does, never composited. Its batches are an opaque
    // single-sided core and alpha-cut two-sided fringe cards, so one set per sidedness.
    let skin_extra = sections
        .0
        .skin_extra_texture(look.race, look.sex, look.skin)
        .map_or((None, None), |path| {
            let tex = asset_server.load::<Image>(format!(
                "mpq://{}",
                path.replace('\\', "/").to_ascii_lowercase()
            ));
            let mut quint_for = |two_sided: bool| {
                let part = parts.iter().find(|p| {
                    p.char_slot == Some(CharSkinSlot::SkinExtra) && p.two_sided == two_sided
                })?;
                mats.char_variants(tex.clone(), part.blend, part.two_sided)
                    .map(&quint)
            };
            (quint_for(false), quint_for(true))
        });

    (body, hair, object, skin_extra)
}
