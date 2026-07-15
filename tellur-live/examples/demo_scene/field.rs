//! 02 / FIELD — a tight 5×3 coordinate grid of breathing dots with row/column
//! labels, swept left-to-right by a bright vertical scan line trailing a soft
//! pink wash.

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;
use tellur_core::text::Weight;
use tellur_core::time::{LocalTime, Time};

use super::common::*;

const ROWS: i32 = 3;
const COLS: i32 = 5;
const SPACING_X: f32 = 220.0;
const SPACING_Y: f32 = 200.0;

#[tellur_core::component(vector)]
pub fn Field(time: LocalTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(1.7, 3.6).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    // Rise-hold-fall: a product of an easing-in and an easing-out factor.
    let life = time.phase(1.9, 2.25).ease_in_out_expo(0.0, 1.0)
        * time.phase(3.2, 3.55).ease_in_out_expo(1.0, 0.0);

    VectorLayer::builder()
        .size(SCENE_SIZE)
        // A tight, deliberate 5×3 grid — fewer dots, more breathing room, more
        // alignment. Columns alternate pink / cyan so the palette reads as
        // intentional pairs rather than a confetti. Each cell is a `Fragment`
        // of its dot plus, for the four corners, an accent outline ring.
        .children((0..ROWS).flat_map(move |r| {
            (0..COLS).map(move |c| {
                let dx = (c as f32 - (COLS as f32 - 1.0) * 0.5) * SPACING_X;
                let dy = (r as f32 - (ROWS as f32 - 1.0) * 0.5) * SPACING_Y;
                let stagger = c as f64 * 0.05 + r as f64 * 0.025;
                let pop = time
                    .phase(1.95 + stagger, 2.45 + stagger)
                    .ease_out_cubic(0.0, 1.0);
                // `ease_in_back` overshoots — the compound visibility `s`
                // is plain f32 arithmetic clamped at the leaf.
                let collapse = time
                    .phase(3.05 + stagger * 0.4, 3.5 + stagger * 0.4)
                    .ease_in_back(0.0, 1.0);
                let s = (pop * (1.0 - collapse)).clamp(0.0, 1.0);

                // Stagger shifts each dot's breathing by a fraction of the
                // 1.6s cycle so the grid doesn't pulse in lockstep. `wave`
                // rises from its trough (1 - cos); the breathing was authored
                // on a sine that starts mid-swing rising, so lead by a
                // quarter period on top of the stagger.
                let breathe = LocalTime::new(time.seconds() + (stagger + 0.25) * 1.6)
                    .wave(1.6)
                    .linear(0.85, 1.15);
                let cx = CX + dx;
                let cy = CY + dy * (1.0 - collapse * 0.45);

                let color = if c % 2 == 0 { p.pink } else { p.cyan };

                Fragment::builder()
                    .child(
                        Circle::builder()
                            .radius(14.0 * breathe * s)
                            .fill(color.with_alpha(life * 0.92))
                            .anchored(Anchor::CENTER)
                            .snap_to(Vec2(cx, cy)),
                    )
                    // The four corner dots get an accent outline ring — that
                    // small hierarchy cue costs nothing and pulls the eye to
                    // the frame.
                    .maybe_child(
                        ((r == 0 || r == ROWS - 1) && (c == 0 || c == COLS - 1)).then(|| {
                            let ring_in = time
                                .phase(2.4 + stagger, 2.9 + stagger)
                                .ease_out_cubic(0.0, 1.0);
                            Circle::builder()
                                .radius(26.0 * ring_in * s)
                                .stroke(Stroke::new(color.with_alpha(life * 0.55), 2.0))
                                .anchored(Anchor::CENTER)
                                .snap_to(Vec2(cx, cy))
                        }),
                    )
                    .build()
            })
        }))
        // Row labels on the left side of the grid — tiny "R00/R01/R02" marks
        // that make the grid feel like a numbered coordinate space rather than
        // just dots. Fade in with the grid itself.
        .children((0..ROWS).map(move |r| {
            let dy = (r as f32 - (ROWS as f32 - 1.0) * 0.5) * SPACING_Y;
            let label_in = time
                .phase(2.1 + r as f64 * 0.04, 2.55 + r as f64 * 0.04)
                .ease_out_cubic(0.0, 1.0);
            let label_out = time.phase(3.1, 3.5).ease_in_back(0.0, 1.0);
            let row_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
            Fragment::builder()
                .maybe_child((row_alpha > 0.0).then(|| {
                    Text::builder()
                        .font(MONOSPACE.clone())
                        .size(12.0)
                        .weight(Weight::NORMAL)
                        .fill(p.paper.with_alpha(row_alpha))
                        .span(TextSpan::plain(format!("R{:02}", r)))
                        .anchored(Anchor::CENTER_RIGHT)
                        .snap_to(Vec2(CX - 720.0, CY + dy))
                }))
                .build()
        }))
        // Column labels along the top of the grid — symmetric with the rows so
        // the grid reads as a proper coordinate field.
        .children((0..COLS).map(move |c| {
            let dx = (c as f32 - (COLS as f32 - 1.0) * 0.5) * SPACING_X;
            let label_in = time
                .phase(2.05 + c as f64 * 0.04, 2.5 + c as f64 * 0.04)
                .ease_out_cubic(0.0, 1.0);
            let label_out = time.phase(3.1, 3.5).ease_in_back(0.0, 1.0);
            let col_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
            Fragment::builder()
                .maybe_child((col_alpha > 0.0).then(|| {
                    Text::builder()
                        .font(MONOSPACE.clone())
                        .size(12.0)
                        .weight(Weight::NORMAL)
                        .fill(p.paper.with_alpha(col_alpha))
                        .span(TextSpan::plain(format!("C{:02}", c)))
                        .anchored(Anchor::BOTTOM_CENTER)
                        .snap_to(Vec2(
                            CX + dx,
                            CY - (ROWS as f32 - 1.0) * 0.5 * SPACING_Y - 38.0,
                        ))
                }))
                .build()
        }))
        // Vertical scan line sweeping left-to-right through the grid. Bright
        // head + dimmer trailing wash, with a small "SCAN" data tag at the top
        // and a running-position readout at the bottom.
        .maybe_child({
            let sweep = time.phase(2.35, 3.0).eased(Easing::InOutExpo);
            (sweep.get() > 0.0 && sweep.get() < 1.0)
                .then(|| ScanLine::builder().palette(p).life(life).sweep(sweep))
        })
        .build()
}

// FIELD's vertical sweep: trailing wash, inner trail, the crisp leading line,
// top + bottom phosphor head dots, and the SCAN / percentage readouts — in
// that paint order.
#[tellur_core::component(vector)]
fn ScanLine(palette: Palette, life: f32, sweep: Phase) -> impl VectorComponent {
    let p = palette;
    let x = sweep.linear(CX - 580.0, CX + 580.0);
    // Hat-shaped visibility curve `4x(1-x)` peaks at 1 when sweep = 0.5 and
    // is 0 at both ends.
    let visibility = peak(sweep.get());
    let height = (ROWS as f32 + 0.3) * SPACING_Y * 0.5 * 2.0;
    let top_y = CY - height * 0.5;
    let bottom_y = top_y + height;

    // Trailing wash — a soft pink band behind the leading line that
    // gives the sweep a feeling of "leaving a trace".
    let trail_w = 120.0;
    let inner_trail_w = 32.0;
    let pct = (sweep.get() * 100.0) as i32;
    let pct_text = format!("{:03}%", pct);
    let head_alpha = visibility * life;

    Fragment::builder()
        .child(
            Rectangle::builder()
                .size(Vec2(trail_w, height))
                .fill(p.pink.with_alpha(head_alpha * 0.14))
                .place_at(Vec2(x - trail_w, top_y)),
        )
        // Inner brighter trail.
        .child(
            Rectangle::builder()
                .size(Vec2(inner_trail_w, height))
                .fill(p.pink.with_alpha(head_alpha * 0.22))
                .place_at(Vec2(x - inner_trail_w, top_y)),
        )
        // The crisp leading line.
        .child(
            Rectangle::builder()
                .size(Vec2(4.0, height))
                .fill(p.pink.with_alpha(head_alpha * 0.95))
                .place_at(Vec2(x - 2.0, top_y)),
        )
        // Bright head dot at the top of the sweep — like a phosphor pixel.
        .child(
            Circle::builder()
                .radius(7.0)
                .fill(p.paper.with_alpha(head_alpha))
                .stroke(Stroke::new(p.pink.with_alpha(head_alpha), 2.0))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(x, top_y)),
        )
        // Mirror head dot at the bottom for symmetry.
        .child(
            Circle::builder()
                .radius(7.0)
                .fill(p.paper.with_alpha(head_alpha))
                .stroke(Stroke::new(p.pink.with_alpha(head_alpha), 2.0))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(x, bottom_y)),
        )
        // Top tag.
        .child(
            Text::builder()
                .font(MONOSPACE.clone())
                .size(14.0)
                .weight(Weight::BOLD)
                .fill(p.pink.with_alpha(head_alpha))
                .span(TextSpan::plain("SCAN →"))
                .anchored(Anchor::BOTTOM_LEFT)
                .snap_to(Vec2(x + 16.0, top_y - 8.0)),
        )
        // Bottom percentage readout — "treats this as data" cue.
        .child(
            Text::builder()
                .font(MONOSPACE.clone())
                .size(13.0)
                .weight(Weight::BOLD)
                .fill(p.paper.with_alpha(head_alpha * 0.95))
                .span(TextSpan::plain(pct_text))
                .anchored(Anchor::TOP_LEFT)
                .snap_to(Vec2(x + 16.0, bottom_y + 8.0)),
        )
        .build()
}
