//! A small gated bridge: reuse the stock bubble verdicts and let addons style actual UI frames.

pub(super) fn sync(
    script: &benilla_ui::script::UiScript,
    bubbles: &std::collections::HashMap<u64, super::Bubble>,
    pending: &[super::Pending],
    scale: f32,
) -> bool {
    if !script
        .eval::<bool>("return C_ChatBubbles ~= nil")
        .unwrap_or(false)
    {
        return false;
    }
    let rows = pending.iter().filter_map(|p| {
        let b = bubbles.get(&p.guid)?;
        let basis = super::plate_basis(p.viewport);
        let x = super::device_snap(p.seat.x, scale);
        Some(manila_pfui::bubbles::Bubble {
            guid: p.guid,
            born: b.born,
            text: &b.text,
            color: b.color,
            alpha: b.alpha,
            visible: true,
            x,
            bottom: p.viewport.y - super::device_snap(p.seat.y, scale),
            wrap: super::gx_px(super::WRAP_W, basis),
            margin: super::gx_px(super::MARGIN, basis),
            border: super::border_px(p.viewport, basis),
            font_height: super::text_px(super::TEXT_H, basis),
        })
    });
    let mut offscreen: Vec<_> = bubbles
        .iter()
        .filter(|(guid, _)| !pending.iter().any(|p| p.guid == **guid))
        .collect();
    offscreen.sort_by_key(|(guid, _)| **guid);
    let rows = rows.chain(
        offscreen
            .into_iter()
            .map(|(guid, b)| manila_pfui::bubbles::Bubble {
                guid: *guid,
                born: b.born,
                text: &b.text,
                color: b.color,
                alpha: b.alpha,
                visible: false,
                x: 0.0,
                bottom: 0.0,
                wrap: 0.0,
                margin: 0.0,
                border: 0.0,
                font_height: 0.0,
            }),
    );
    if let Err(error) = manila_pfui::bubbles::sync(script.lua(), rows) {
        script.report_script_error(&format!("pfUI chat bubbles: {error}"));
    }
    true
}
