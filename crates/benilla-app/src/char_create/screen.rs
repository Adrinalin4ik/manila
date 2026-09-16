//! The create screen's **layout** — the reference `CharacterCreate.xml` arrangement rebuilt in
//! Bevy UI (decision 0423's polish passes), full-bleed and scaled to the window: the glue engine
//! renders a 1024×768 virtual screen scaled to the display, so every authored offset/size below is
//! the ref's number times `height / 768`. The widget shapes it places live in [`super::widgets`],
//! the component vocabulary in [`super::parts`], the art in [`super::art`], and the systems that
//! drive it all in [`super::refresh`].
//!
//! Left: the configuration tower — `UI-CharacterCreate-Background` under three stacked
//! `OuterBorder` pieces, the faction banners behind the 2×4 race grid, the gender pair, the
//! valid-classes-only grid, the five `LabelFrame` dial spinners, Randomize. Right: the three
//! `TextPanel-Border` info panels (faction/race/class), bg-tinted per faction like the ref's
//! `SetBackdropColor`, quoting the GlueStrings paragraphs. Center: the transparent-booth model,
//! full-height, drag- or button-rotatable (the big `UI-RotationRight` pair, bottom-left). Bottom:
//! NAME over the `Glue-Tooltip-Border` edit box; Accept over Back in the corner.

use bevy::prelude::*;
use bevy::ui_render::ui_material::MaterialNode;
use bevy::window::PrimaryWindow;

use crate::char_select::{race_name, wow_font};
use crate::entities::CharCreate;
use crate::glue_strings::GlueStrings;
use crate::portrait::{GlueLook, GluePreview, PortraitImages, PortraitSource, GLUE_SLOT};
use benilla_assets::WorldAssets;

use super::parts::{CharCreateUi, DialRow, DynIcon, DynText, DynTint, StatusLine};
use super::{CreateAction, CreateSelection, ALLIANCE, HORDE, INITIAL_FACING};
use crate::glue::art::{
    tc_rect, GlueArt, ALLIANCE_BORDER, ALLIANCE_FILL, BACKDROP, BTN_BG, DIM, GOLD, INFO_TEXT,
};
use crate::glue::widgets::{
    abs, dial_arrow, glue_button, icon_button, outlined_text, ArtSwap, FallbackFace, GlueBtnKind,
    GlueText, Hilight,
};

const SCREEN_Z: i32 = 1100;

// ── Spawn ────────────────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn enter_create(
    mut commands: Commands,
    assets: Res<AssetServer>,
    catalog: Option<Res<CharCreate>>,
    portraits: Res<PortraitImages>,
    mut sel: ResMut<CreateSelection>,
    mut preview: ResMut<GluePreview>,
    mut art: ResMut<GlueArt>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut add_mats: ResMut<Assets<crate::glue::add_material::AddUiMaterial>>,
    strings: Option<Res<GlueStrings>>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    sel.reset(catalog.as_deref());
    preview.scene = Some(crate::portrait::GlueScene::Race(sel.race));
    preview.look = Some(GlueLook::Create(sel.look()));
    preview.yaw = INITIAL_FACING;
    if let Some(mut wa) = world_assets {
        art.ensure_loaded(&mut wa, &mut images, &mut add_mats);
    }
    spawn_screen(
        &mut commands,
        &assets,
        &portraits,
        &art,
        strings.as_deref(),
        catalog.as_deref(),
        &window,
    );
}

/// Rebuild the tree when a window resize (mac fullscreen, a drag) has changed the glue scale it
/// was baked at — the create screen has no per-frame materialize (it spawns on entry, after the
/// boot-order traps the other screens dodge), so the rescale watch lives here. Selection and the
/// typed name live in resources and repaint via [`super::refresh`], so the rebuild loses nothing.
pub(super) fn rescale_screen(
    mut commands: Commands,
    existing: Query<(Entity, &CharCreateUi)>,
    assets: Res<AssetServer>,
    portraits: Res<PortraitImages>,
    art: Res<GlueArt>,
    strings: Option<Res<GlueStrings>>,
    catalog: Option<Res<CharCreate>>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    let s = crate::glue::screen_scale(window.single().ok());
    for (root, ui) in &existing {
        if ui.s != s {
            commands.entity(root).despawn();
            spawn_screen(
                &mut commands,
                &assets,
                &portraits,
                &art,
                strings.as_deref(),
                catalog.as_deref(),
                &window,
            );
        }
    }
}

fn spawn_screen(
    commands: &mut Commands,
    assets: &AssetServer,
    portraits: &PortraitImages,
    art: &GlueArt,
    strings: Option<&GlueStrings>,
    catalog: Option<&CharCreate>,
    window: &Query<&Window, With<PrimaryWindow>>,
) {
    let font = wow_font(assets);
    // The edit box types in `GlueEditBoxFont` — ARIALN, not FRIZQT (GlueFonts.xml).
    let edit_font: Handle<Font> = assets.load("mpq://Fonts/ARIALN.ttf");
    let model_image = match portraits.0.get(GLUE_SLOT) {
        Some(PortraitSource::Live(h)) => Some(h.clone()),
        _ => None,
    };
    // The glue engine scales a 1024×768 virtual screen to the window; scale the authored sizes the
    // same way so the ref proportions hold at any size.
    let s = crate::glue::screen_scale(window.single().ok());
    let px = |v: f32| Val::Px(v * s);
    let empty = GlueStrings::default();
    let strings = strings.unwrap_or(&empty);

    let root = commands
        .spawn((
            CharCreateUi { s },
            DynTint::Backdrop,
            GlobalZIndex(SCREEN_Z),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(BACKDROP),
        ))
        .with_children(|ui| {
            // The 3D scene, full-bleed and first (everything else draws over it) — the ref's
            // screen IS a fullscreen ModelFFX: the per-race background with the character standing
            // in it (the booth renders both into this window-sized target). The whole pane drags
            // to rotate, the ref's full-frame mouse rotation; the page tint behind it is the
            // no-art fallback. It keeps the WINDOW while the chrome below does not: a pillarbox's
            // bars are the booth camera's own output clear inside this same target (1619 §3).
            let mut pane = ui.spawn((
                CreateAction::Model,
                Button,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
            ));
            if let Some(image) = model_image {
                pane.insert(ImageNode::new(image));
            }
        })
        .id();

    // ...and every piece of chrome hangs off the CANVAS — the boxed scene's own rect (decision
    // 2091). The race/class towers are this screen's edge-anchored chrome: against the window they
    // stand over the bars, and 1587's "no void at 21:9" held only because they did (1619 §2).
    let mut canvas = commands.spawn((crate::glue::glue_canvas(), ChildOf(root)));
    canvas.with_children(|ui| {
        left_tower(ui, art, &font, s, strings, catalog);

        // The WoW logo (`CharacterCreateWoWLogo`, 256×128 at (3,−7)) — after the tower, like the
        // ref's frame order (child frames draw over the parent's border art).
        if let Some(logo) = &art.logo {
            ui.spawn((ImageNode::new(logo.clone()), abs(s, 3.0, 7.0, 256.0, 128.0)));
        }

        super::panels::right_stack(ui, art, &font, s);
        name_cluster(ui, art, &font, &edit_font, s, strings);
        rotate_cluster(ui, art, &font, s);

        // Accept over Back, bottom-right (`CharCreateOkayButton` 160×35 over `BackButton` 120×30
        // at BOTTOMRIGHT (−50, 20)).
        ui.spawn((Node {
            position_type: PositionType::Absolute,
            right: px(50.0),
            bottom: px(20.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: px(5.0),
            ..default()
        },))
            .with_children(|actions| {
                glue_button(
                    actions,
                    art,
                    &font,
                    CreateAction::Create,
                    strings.text("CHARACTER_CREATE_ACCEPT", "Accept"),
                    160.0,
                    35.0,
                    GlueBtnKind::Normal,
                    s,
                );
                glue_button(
                    actions,
                    art,
                    &font,
                    CreateAction::Back,
                    strings.text("BACK", "Back"),
                    120.0,
                    30.0,
                    GlueBtnKind::Small,
                    s,
                );
            });
    });
}

/// How tall the race grid actually is, and what the rest of the tower has to do about it.
///
/// **The reference authors nothing below the race grid as a constant.** Every frame under it hangs
/// off the one above by a relative anchor, and the chain starts at the LAST race slot:
/// `CharacterCreateGenderButtonMale` is `RaceButton5.BOTTOMLEFT + (27,−25)`, `ClassButton1` is
/// `GenderMale.BOTTOMLEFT + (−44,−15)`, `CustomizationButtonFrame1` is `ClassButton6.BOTTOM +
/// (20,−15)`, and `CharCreateRandomizeButton` is re-anchored to `CustomizationButtonFrame5.BOTTOM
/// + (0,−5)` on every race change (`CharacterCreate.lua:386`). Read out of this install's own
/// `Interface\GlueXML\CharacterCreate.xml` through the host's `/data` route.
///
/// This screen flattened that chain into four authored tops — 303 / 369 / 480 / 645 — every one of
/// them measured from a **four**-row column. The grid itself grows with the race count, so a fifth
/// race walked straight into the gender row and shoved the rest off the bottom. That is the defect;
/// this type is the chain put back.
///
/// **Vanilla is unchanged by construction, not by care.** At four rows `shift` is exactly zero and
/// `tower_top` clamps to the authored 74, so every number this produces is the number that was
/// there before. There is no second layout to keep in step.
struct TowerRows {
    /// Race-icon edge. 48 is this screen's authored vanilla value; 45 is what a ten-race install
    /// uses (`CharacterCreateIconButtonTemplate`, same file) — the reference shrinks the icon
    /// rather than let five rows push the tower up past its own banner art.
    icon: f32,
    /// How far everything below the grid moves. Zero on vanilla.
    shift: f32,
    /// The tower's own top. Raised only as far as the overflow demands — never below the authored
    /// 74, and never above 0.
    tower_top: f32,
    /// How tall the banner texture is drawn. Another install-authored number rather than a
    /// derivation — 259 in this screen's vanilla reading, 500 in a ten-race client's own XML.
    banner_h: f32,
}

impl TowerRows {
    /// The glue engine's virtual canvas height; the tower is laid out in these units.
    const CANVAS_H: f32 = 768.0;
    /// The authored numbers this screen has always used, and the shape of the stack under the
    /// grid: `RANDOMIZE_TOP` is the last thing in the tower and `30` is its height.
    const VANILLA_ICON: f32 = 48.0;
    const GAP: f32 = 5.0;
    const VANILLA_ROWS: f32 = 4.0;
    const RANDOMIZE_TOP: f32 = 645.0;
    const RANDOMIZE_H: f32 = 30.0;
    const VANILLA_BANNER_H: f32 = 259.0;

    fn of(catalog: Option<&CharCreate>) -> Self {
        // The taller of the two columns, counting only races the catalog can actually describe —
        // the same filter the grid itself applies, so the budget and the art cannot disagree.
        let offered = |faction: &[u8]| {
            faction
                .iter()
                .filter(|r| catalog.and_then(|c| c.0.race_file(**r)).is_some())
                .count() as f32
        };
        Self::for_rows(offered(&ALLIANCE).max(offered(&HORDE)).max(1.0))
    }

    /// The arithmetic alone, so it can be checked without a loaded DBC.
    fn for_rows(rows: f32) -> Self {
        let icon = if rows > Self::VANILLA_ROWS {
            45.0
        } else {
            Self::VANILLA_ICON
        };
        let span = |rows: f32, icon: f32| rows * icon + (rows - 1.0) * Self::GAP;
        // **Never negative.** A grid SHORTER than four rows must not pull the stack up: the
        // reference's hidden race slots keep their anchored positions, so its gender row sits below
        // slot 5 whether or not slot 5 is drawn, and compacting would be our invention rather than
        // its behaviour. No real install reaches this leg — `rows` is the taller column — but a
        // derivation that only happens to be right on real data is not a derivation.
        let shift = (span(rows, icon) - span(Self::VANILLA_ROWS, Self::VANILLA_ICON)).max(0.0);
        // Raise the tower by exactly what hangs off the bottom, and not one unit more. **This is
        // the whole reason there is no second magic number here**: on a ten-race install it works
        // out to 55, which is precisely where that install's own XML puts the frame
        // (`CharacterCreateConfigurationFrame` at TOPLEFT (28,−55) against vanilla's −74). The rule
        // reproduces the reference's answer instead of copying it, which is the only evidence
        // available that the rule is the right one.
        let bottom = Self::RANDOMIZE_TOP + shift + Self::RANDOMIZE_H;
        let tower_top = 74.0_f32.min(Self::CANVAS_H - bottom).max(0.0);
        Self {
            icon,
            shift,
            tower_top,
            banner_h: if rows > Self::VANILLA_ROWS {
                500.0
            } else {
                Self::VANILLA_BANNER_H
            },
        }
    }

    /// An authored top from the four-row era, moved by whatever the grid actually costs.
    fn below_grid(&self, authored: f32) -> f32 {
        authored + self.shift
    }
}

/// The configuration tower (`CharacterCreateConfigurationFrame`, 206×600 at TOPLEFT (28,−74) on
/// vanilla; [`TowerRows`] moves it up when a longer race grid demands it): frame art, banners,
/// faction headers, the race/gender/class grids, the dial rows, Randomize.
fn left_tower(
    ui: &mut ChildSpawnerCommands,
    art: &GlueArt,
    font: &Handle<Font>,
    s: f32,
    strings: &GlueStrings,
    catalog: Option<&CharCreate>,
) {
    let px = |v: f32| Val::Px(v * s);

    // ── The tower's vertical budget, derived rather than authored (see `TowerRows`) ──────────
    let rows = TowerRows::of(catalog);
    ui.spawn((Node {
        position_type: PositionType::Absolute,
        left: px(28.0),
        top: px(rows.tower_top),
        width: px(206.0),
        height: px(600.0),
        ..default()
    },))
        .with_children(|tower| {
            // The frame: `UI-CharacterCreate-Background` stretched behind (TOPLEFT of border1 +6 →
            // BOTTOMLEFT of border3 +6), three `OuterBorder` pieces stacked over it (224 wide,
            // centered on the 206 frame → x −9; heights 236/240/210 with the authored texcoords).
            if let Some(bg) = &art.tower_bg {
                tower.spawn((ImageNode::new(bg.clone()), abs(s, -3.0, 0.0, 218.0, 680.0)));
            }
            if let Some((border, size)) = &art.tower_border {
                for (top, height, tc) in [
                    (0.0, 236.0, [0.0, 0.875, 0.0, 0.9375]),
                    (236.0, 240.0, [0.0, 0.875, 0.0, 0.9375]),
                    (476.0, 210.0, [0.0, 0.875, 0.1796875, 1.0]),
                ] {
                    tower.spawn((
                        ImageNode {
                            image: border.clone(),
                            rect: Some(tc_rect(*size, tc)),
                            ..default()
                        },
                        abs(s, -9.0, top, 224.0, height),
                    ));
                }
            }
            // The banners (`CharacterCreateBanners`, 256×259 at TOP (−2,−60)) behind the race grid.
            //
            // The HEIGHT is an install's own number, not a derivation: vanilla's texture is drawn
            // 259 tall and a ten-race client draws the same file 500 tall (`CharacterCreateBanners`
            // in its `CharacterCreate.xml`), stretching the painted banner down past the longer
            // grid and behind the class icons. Ours stayed at 259, which is why the blue and the
            // red stopped two rows short of the races standing on them.
            if let Some(banners) = &art.banners {
                tower.spawn((
                    ImageNode::new(banners.clone()),
                    abs(s, -27.0, 60.0, 256.0, rows.banner_h),
                ));
            }
            // Alliance | Horde over the banner tops (bottom-anchored ±50 of the banner center).
            // The XML's `text="ALLIANCE"` is a localization key — GlueStrings renders it
            // mixed-case ("Alliance"), never the raw key.
            for (key, fallback, center) in
                [("ALLIANCE", "Alliance", 51.0), ("HORDE", "Horde", 151.0)]
            {
                outlined_text(
                    tower,
                    Node {
                        justify_content: JustifyContent::Center,
                        ..abs(s, center - 60.0, 40.0, 120.0, 17.0)
                    },
                    (),
                    (),
                    GlueText {
                        text: strings.text(key, fallback),
                        size: 15.0, // GlueFontNormal
                        color: GOLD,
                        wrap: false,
                    },
                    font,
                    s,
                );
            }

            // The race grid: two columns of 48² check-buttons (col A at (33,68), col B at (127,68),
            // row pitch 48+5). The columns list every race the engine enumerates — Turtle's two
            // additions included — filtered to what the loaded catalog actually offers, so a
            // vanilla install keeps its four-per-column layout.
            for (faction, left) in [(ALLIANCE, 33.0), (HORDE, 127.0)] {
                tower
                    .spawn((Node {
                        position_type: PositionType::Absolute,
                        left: px(left),
                        top: px(68.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(5.0),
                        ..default()
                    },))
                    .with_children(|col| {
                        for race in faction
                            .iter()
                            .copied()
                            .filter(|r| catalog.and_then(|c| c.0.race_file(*r)).is_some())
                        {
                            icon_button(
                                col,
                                font,
                                CreateAction::Race(race),
                                Some(DynIcon::Race(race)),
                                None,
                                None::<DynText>,
                                race_name(race),
                                art,
                                rows.icon,
                                s,
                            );
                        }
                    });
            }

            // The gender pair (below race col A: race4's BOTTOMLEFT + (20,−28)).
            tower
                .spawn((Node {
                    position_type: PositionType::Absolute,
                    left: px(53.0),
                    top: px(rows.below_grid(303.0)),
                    flex_direction: FlexDirection::Row,
                    column_gap: px(5.0),
                    ..default()
                },))
                .with_children(|g| {
                    for (sex, key, fallback) in [(0u8, "MALE", "Male"), (1u8, "FEMALE", "Female")] {
                        let icon = art.gender.as_ref().map(|(h, size)| {
                            let half = if sex == 0 {
                                [0.0, 0.5, 0.0, 1.0]
                            } else {
                                [0.5, 1.0, 0.0, 1.0]
                            };
                            (h.clone(), tc_rect(*size, half))
                        });
                        icon_button(
                            g,
                            font,
                            CreateAction::Gender(sex),
                            None::<DynIcon>,
                            icon,
                            None::<DynText>,
                            strings.text(key, fallback),
                            art,
                            48.0,
                            s,
                        );
                    }
                });

            // The class grid (3-wide under the banners: cols at x 27/79/131, rows touching) — 8
            // slots refreshed to the selected race's valid classes; unused slots collapse (the
            // ref's enumerate-then-hide compacts the same way).
            tower
                .spawn((Node {
                    position_type: PositionType::Absolute,
                    left: px(27.0),
                    top: px(rows.below_grid(369.0)),
                    // **Two units of slack, and they are load-bearing.** Three 48s and two 4s come
                    // to exactly 152, so the authored width was an EXACT fit — and an exact fit is
                    // the one case flexbox cannot be trusted with, because the row's total and the
                    // container's width are computed from `48.0 * s` and `152.0 * s` by different
                    // multiplications. One ulp the wrong way wraps the third icon onto its own row,
                    // which is what a class grid two-wide instead of three-wide was: not a layout
                    // opinion, a float comparison. The slack is far short of a fourth column (52
                    // more), so the grid cannot silently become four-wide either.
                    width: px(3.0 * 48.0 + 2.0 * 4.0 + 2.0),
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: px(4.0),
                    ..default()
                },))
                .with_children(|grid| {
                    for slot in 0..8u8 {
                        icon_button(
                            grid,
                            font,
                            CreateAction::ClassSlot(slot),
                            Some(DynIcon::ClassSlot(slot)),
                            None,
                            Some(DynText::ClassSlotLabel(slot)),
                            "",
                            art,
                            48.0,
                            s,
                        );
                    }
                });

            // The five dial spinners (`CharacterCustomizationFrameTemplate`, 198×32 rows stacked
            // from (4,480) — centered under the class grid).
            tower
                .spawn((Node {
                    position_type: PositionType::Absolute,
                    left: px(4.0),
                    top: px(rows.below_grid(480.0)),
                    width: px(198.0),
                    flex_direction: FlexDirection::Column,
                    ..default()
                },))
                .with_children(|dials| {
                    for dial in 0..5u8 {
                        dial_row(dials, art, font, dial, s);
                    }
                });

            // RANDOMIZE (146×30, centered on the row column). The XML anchors it 25 below the
            // dials, but `CharacterCreate_UpdateFacialHairCustomization` re-anchors it on every
            // race set — `SetPoint("TOP", Frame5, "BOTTOM", 0, -5)` — so the shipped client always
            // shows the 5px gap.
            tower
                .spawn((Node {
                    position_type: PositionType::Absolute,
                    left: px(30.0),
                    top: px(rows.below_grid(645.0)),
                    ..default()
                },))
                .with_children(|r| {
                    glue_button(
                        r,
                        art,
                        font,
                        CreateAction::Randomize,
                        strings.text("RANDOMIZE", "Randomize"),
                        146.0,
                        30.0,
                        GlueBtnKind::Small,
                        s,
                    );
                });
        });
}

/// One dial spinner row: the `CharacterCreate-LabelFrame` 3-slice (64-tall art overhanging the
/// 32-tall row), the centered per-race label, and the 32² arrow pair on the right.
fn dial_row(
    dials: &mut ChildSpawnerCommands,
    art: &GlueArt,
    font: &Handle<Font>,
    dial: u8,
    s: f32,
) {
    let px = |v: f32| Val::Px(v * s);
    dials
        .spawn((
            DialRow(dial),
            Node {
                width: px(198.0),
                height: px(32.0),
                ..default()
            },
        ))
        .with_children(|row| {
            // LabelFrame: Left 25 at (−5), Middle stretched, Right 25 ending at x 154 (RIGHT −44,
            // clearing the arrows) — the 128×64 art's 25|78|25 horizontal slices.
            if let Some((frame, size)) = &art.label_frame {
                for (left, width, tc) in [
                    (-5.0, 25.0, [0.0, 0.1953125, 0.0, 1.0]),
                    (20.0, 109.0, [0.1953125, 0.8046875, 0.0, 1.0]),
                    (129.0, 25.0, [0.8046875, 1.0, 0.0, 1.0]),
                ] {
                    row.spawn((
                        ImageNode {
                            image: frame.clone(),
                            rect: Some(tc_rect(*size, tc)),
                            ..default()
                        },
                        abs(s, left, -16.0, width, 64.0),
                    ));
                }
            }
            // The per-race label, centered on the frame middle (`GlueFontHighlightSmall`).
            outlined_text(
                row,
                Node {
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..abs(s, 20.0, 0.0, 109.0, 32.0)
                },
                (),
                DynText::DialLabel(dial),
                GlueText {
                    text: "",
                    size: 12.0,
                    color: INFO_TEXT,
                    wrap: false,
                },
                font,
                s,
            );
            dial_arrow(
                row,
                &art.arrow_left,
                font,
                CreateAction::Dial(dial, -1),
                137.0,
                "<",
                s,
            );
            dial_arrow(
                row,
                &art.arrow_right,
                font,
                CreateAction::Dial(dial, 1),
                166.0,
                ">",
                s,
            );
        });
}

/// NAME over the edit box (`CharacterCreateNameEdit`, 156×40 at BOTTOM (8,50), backdropped in
/// `Glue-Tooltip-Border` — always Alliance-tinted, the ref's `OnLoad`), our status line beneath.
fn name_cluster(
    ui: &mut ChildSpawnerCommands,
    art: &GlueArt,
    font: &Handle<Font>,
    edit_font: &Handle<Font>,
    s: f32,
    strings: &GlueStrings,
) {
    let px = |v: f32| Val::Px(v * s);
    ui.spawn((Node {
        position_type: PositionType::Absolute,
        bottom: px(50.0),
        left: px(8.0),
        width: Val::Percent(100.0),
        flex_direction: FlexDirection::Column,
        align_items: AlignItems::Center,
        row_gap: px(2.0),
        ..default()
    },))
        .with_children(|cluster| {
            outlined_text(
                cluster,
                Node::default(),
                (),
                (),
                GlueText {
                    text: strings.text("NAME", "Name"),
                    size: 18.0, // GlueFontNormalLarge
                    color: GOLD,
                    wrap: false,
                },
                font,
                s,
            );
            // The shared glue edit-box chrome (decision 0539) — Alliance-tinted, always (the
            // ref's `OnLoad`); the create refresh writes the typed name into the marker.
            crate::glue::widgets::glue_edit_box(
                cluster,
                art,
                edit_font,
                (),
                DynText::Name,
                (156.0, 40.0),
                (ALLIANCE_BORDER, ALLIANCE_FILL),
                (15.0, 0.0, 0.0, 0.0), // CharacterCreate.xml: TextInsets left 15 only
                s,
            );
            // Empty until a create fails — the ref surfaces errors in a dialog; this line is our
            // minimal stand-in, never an idle hint.
            outlined_text(
                cluster,
                Node::default(),
                (),
                StatusLine,
                GlueText {
                    text: "",
                    size: 13.0,
                    color: DIM,
                    wrap: true,
                },
                font,
                s,
            );
        });
}

/// The rotate pair (`CharacterCreateRotateLeft/Right`: 50² at BOTTOMLEFT (237,0), overlapping
/// −19) — `UI-RotationRight-Big` art, the left button mirrored, `UI-Common-MouseHilight` on hover.
fn rotate_cluster(ui: &mut ChildSpawnerCommands, art: &GlueArt, font: &Handle<Font>, s: f32) {
    let px = |v: f32| Val::Px(v * s);
    ui.spawn((Node {
        position_type: PositionType::Absolute,
        left: px(237.0),
        bottom: px(0.0),
        flex_direction: FlexDirection::Row,
        ..default()
    },))
        .with_children(|rot| {
            for (action, flip, overlap) in [
                (CreateAction::RotateLeft, true, 0.0),
                (CreateAction::RotateRight, false, -19.0),
            ] {
                let mut b = rot.spawn((
                    action,
                    Button,
                    Node {
                        width: px(50.0),
                        height: px(50.0),
                        margin: UiRect::left(px(overlap)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                ));
                match (&art.rotate_up, &art.rotate_down) {
                    (Some(up), down) => {
                        b.insert(ImageNode {
                            image: up.clone(),
                            flip_x: flip,
                            ..default()
                        });
                        if let Some(down) = down {
                            b.insert(ArtSwap {
                                up: up.clone(),
                                down: down.clone(),
                            });
                        }
                        if let Some(hi) = &art.mouse_hilight {
                            b.with_children(|inner| {
                                inner.spawn((
                                    Hilight,
                                    Visibility::Hidden,
                                    MaterialNode(hi.clone()),
                                    abs(s, 10.0, 10.0, 30.0, 30.0),
                                ));
                            });
                        }
                    }
                    (None, _) => {
                        b.insert((FallbackFace, BackgroundColor(BTN_BG)));
                        b.with_children(|inner| {
                            inner.spawn((
                                Text::new(if flip { "<" } else { ">" }),
                                TextFont {
                                    font: font.clone(),
                                    font_size: 16.0 * s,
                                    ..default()
                                },
                                TextColor(GOLD),
                            ));
                        });
                    }
                }
            }
        });
}

pub(super) fn exit_create(
    mut commands: Commands,
    roots: Query<Entity, With<CharCreateUi>>,
    mut preview: ResMut<GluePreview>,
) {
    for e in &roots {
        commands.entity(e).despawn();
    }
    // Clear the booth + scene. Back to CharSelect re-establishes both the same frame
    // (its `OnEnter` runs after this `OnExit`).
    preview.look = None;
    preview.scene = None;
}

#[cfg(test)]
mod tower_rows_tests {
    use super::TowerRows;

    /// **Backward compatibility is the assertion, not a side note.** A four-row install — every
    /// vanilla one — must come out of this arithmetic with the numbers the screen was authored
    /// with: no shift at all, the authored icon, the authored tower top. If this ever fails, the
    /// derivation has started rewriting a layout that was already right.
    ///
    /// The five-row leg is checked against the reference rather than against itself: a ten-race
    /// install's own `CharacterCreate.xml` puts `CharacterCreateConfigurationFrame` at TOPLEFT
    /// (28,−55), and the clamp is asked to produce 55 without having been told it. That is the
    /// only independent evidence available here that the rule is right, so it is what the test
    /// pins — a self-consistent check (does the shift equal the span difference?) would pass for a
    /// wrong rule too.
    #[test]
    fn vanilla_is_untouched_and_five_rows_land_where_the_client_puts_them() {
        let v = TowerRows::for_rows(4.0);
        assert_eq!((v.shift, v.icon, v.tower_top), (0.0, 48.0, 74.0));
        assert_eq!(v.banner_h, 259.0, "vanilla banner art must not stretch");
        for authored in [303.0, 369.0, 480.0, 645.0] {
            assert_eq!(v.below_grid(authored), authored, "vanilla offset moved");
        }

        // A grid shorter than the authored four must not pull the stack up either — the reference
        // keeps its hidden slots' positions, so there is nothing to compact.
        let short = TowerRows::for_rows(1.0);
        assert_eq!((short.shift, short.tower_top), (0.0, 74.0));

        let t = TowerRows::for_rows(5.0);
        assert_eq!(t.icon, 45.0);
        assert_eq!(t.shift, 38.0, "5×45+4×5 against 4×48+3×5");
        assert_eq!(t.tower_top, 55.0, "the install's own XML says −55");
        assert_eq!(t.banner_h, 500.0, "and 500 for the banner");
        // And the thing the whole change exists to prevent: Randomize inside the canvas.
        assert!(t.tower_top + t.below_grid(645.0) + 30.0 <= TowerRows::CANVAS_H);
    }
}
