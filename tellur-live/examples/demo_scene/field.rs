//! 02 / FIELD — a tight 5×3 coordinate grid of breathing dots with row/column
//! labels, swept left-to-right by a bright vertical scan line trailing a soft
//! pink wash.

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::text::Weight;
use tellur_core::time::{Time, TimelineTime};

use super::common::*;

const ROWS: i32 = 3;
const COLS: i32 = 5;
const SPACING_X: f32 = 220.0;
const SPACING_Y: f32 = 200.0;

#[tellur_core::component(vector)]
pub fn Field(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(1.7, 3.6).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    let life = envelope(
        time,
        (1.9, 2.25),
        (3.2, 3.55),
        |p| p.ease_in_out_expo().get(),
        |p| p.ease_in_out_expo().get(),
    );

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
                let stagger = c as f32 * 0.05 + r as f32 * 0.025;
                let pop = time
                    .phase(1.95 + stagger, 2.45 + stagger)
                    .ease_out_cubic()
                    .get();
                let collapse = time
                    .phase(3.05 + stagger * 0.4, 3.5 + stagger * 0.4)
                    .ease_in_back_between(0.0, 1.0);
                let s = (pop * (1.0 - collapse)).clamp(0.0, 1.0);

                let breathe = 1.0 + wave(time, 1.6, stagger) * 0.15;
                let cx = CX + dx;
                let cy = CY + dy * (1.0 - collapse * 0.45);

                let color = if c % 2 == 0 { p.pink } else { p.cyan };

                Fragment::builder()
                    .child(
                        Circle::builder()
                            .center(Vec2(cx, cy))
                            .radius(14.0 * breathe * s)
                            .fill(alpha(color, life * 0.92)),
                    )
                    // The four corner dots get an accent outline ring — that
                    // small hierarchy cue costs nothing and pulls the eye to
                    // the frame.
                    .maybe_child(
                        ((r == 0 || r == ROWS - 1) && (c == 0 || c == COLS - 1)).then(|| {
                            let ring_in = time
                                .phase(2.4 + stagger, 2.9 + stagger)
                                .ease_out_cubic()
                                .get();
                            Circle::builder()
                                .center(Vec2(cx, cy))
                                .radius(26.0 * ring_in * s)
                                .stroke(alpha(color, life * 0.55))
                                .stroke_width(2.0)
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
                .phase(2.1 + r as f32 * 0.04, 2.55 + r as f32 * 0.04)
                .ease_out_cubic()
                .get();
            let label_out = time.phase(3.1, 3.5).ease_in_back_between(0.0, 1.0);
            let row_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
            Fragment::builder()
                .maybe_child((row_alpha > 0.0).then(|| {
                    Label::builder()
                        .position(Vec2(CX - 720.0, CY + dy))
                        .anchor(Anchor::CENTER_RIGHT)
                        .text(format!("R{:02}", r))
                        .size(12.0)
                        .color(alpha(p.paper, row_alpha))
                        .weight(Weight::NORMAL)
                }))
                .build()
        }))
        // Column labels along the top of the grid — symmetric with the rows so
        // the grid reads as a proper coordinate field.
        .children((0..COLS).map(move |c| {
            let dx = (c as f32 - (COLS as f32 - 1.0) * 0.5) * SPACING_X;
            let label_in = time
                .phase(2.05 + c as f32 * 0.04, 2.5 + c as f32 * 0.04)
                .ease_out_cubic()
                .get();
            let label_out = time.phase(3.1, 3.5).ease_in_back_between(0.0, 1.0);
            let col_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
            Fragment::builder()
                .maybe_child((col_alpha > 0.0).then(|| {
                    Label::builder()
                        .position(Vec2(
                            CX + dx,
                            CY - (ROWS as f32 - 1.0) * 0.5 * SPACING_Y - 38.0,
                        ))
                        .anchor(Anchor::BOTTOM_CENTER)
                        .text(format!("C{:02}", c))
                        .size(12.0)
                        .color(alpha(p.paper, col_alpha))
                        .weight(Weight::NORMAL)
                }))
                .build()
        }))
        // Vertical scan line sweeping left-to-right through the grid. Bright
        // head + dimmer trailing wash, with a small "SCAN" data tag at the top
        // and a running-position readout at the bottom.
        .maybe_child({
            let sweep = time.phase(2.35, 3.0).ease_in_out_expo().get();
            (sweep > 0.0 && sweep < 1.0)
                .then(|| ScanLine::builder().palette(p).life(life).sweep(sweep))
        })
        .build()
}

// FIELD's vertical sweep: trailing wash, inner trail, the crisp leading line,
// top + bottom phosphor head dots, and the SCAN / percentage readouts — in
// that paint order.
#[tellur_core::component(vector)]
fn ScanLine(palette: Palette, life: f32, sweep: f32) -> impl VectorComponent {
    let p = palette;
    let x = lerp(CX - 580.0, CX + 580.0, sweep);
    let visibility = 4.0 * sweep * (1.0 - sweep);
    let height = (ROWS as f32 + 0.3) * SPACING_Y * 0.5 * 2.0;
    let top_y = CY - height * 0.5;
    let bottom_y = top_y + height;

    // Trailing wash — a soft pink band behind the leading line that
    // gives the sweep a feeling of "leaving a trace".
    let trail_w = 120.0;
    let inner_trail_w = 32.0;
    let pct = (sweep * 100.0) as i32;
    let pct_text = format!("{:03}%", pct);

    Fragment::builder()
        .child(
            Rect::builder()
                .position(Vec2(x - trail_w, top_y))
                .size(Vec2(trail_w, height))
                .color(alpha(p.pink, visibility * life * 0.14)),
        )
        // Inner brighter trail.
        .child(
            Rect::builder()
                .position(Vec2(x - inner_trail_w, top_y))
                .size(Vec2(inner_trail_w, height))
                .color(alpha(p.pink, visibility * life * 0.22)),
        )
        // The crisp leading line.
        .child(
            Rect::builder()
                .position(Vec2(x - 2.0, top_y))
                .size(Vec2(4.0, height))
                .color(alpha(p.pink, visibility * life * 0.95)),
        )
        // Bright head dot at the top of the sweep — like a phosphor pixel.
        .child(
            Circle::builder()
                .center(Vec2(x, top_y))
                .radius(7.0)
                .fill(alpha(p.paper, visibility * life))
                .stroke(alpha(p.pink, visibility * life))
                .stroke_width(2.0),
        )
        // Mirror head dot at the bottom for symmetry.
        .child(
            Circle::builder()
                .center(Vec2(x, bottom_y))
                .radius(7.0)
                .fill(alpha(p.paper, visibility * life))
                .stroke(alpha(p.pink, visibility * life))
                .stroke_width(2.0),
        )
        // Top tag.
        .child(
            Label::builder()
                .position(Vec2(x + 16.0, top_y - 8.0))
                .anchor(Anchor::BOTTOM_LEFT)
                .text("SCAN →")
                .size(14.0)
                .color(alpha(p.pink, visibility * life))
                .weight(Weight::BOLD),
        )
        // Bottom percentage readout — "treats this as data" cue.
        .child(
            Label::builder()
                .position(Vec2(x + 16.0, bottom_y + 8.0))
                .anchor(Anchor::TOP_LEFT)
                .text(pct_text)
                .size(13.0)
                .color(alpha(p.paper, visibility * life * 0.95))
                .weight(Weight::BOLD),
        )
        .build()
}
