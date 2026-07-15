//! [`Clock`] — the two time axes (global / local) a component is sampled
//! with, plus the borrowed trigger table.

use std::sync::OnceLock;

use crate::time::{LocalTime, Time, TimelineTime};
use crate::window::Window;

use super::*;

// ── The clock a component is sampled with ────────────────────────────────────

/// What `#[clock]` injects — BOTH time axes for this frame, plus a borrowed,
/// read-only handle to the resolved [`TriggerTable`] so [`Event`] queries can
/// resolve their id.
///
/// `Clock<'a>` borrows the trigger table (`.sketch/02 §8`): the resolved tree
/// owns the one [`TriggerTable`] by value and lends a `&` to each frame's
/// clock, which keeps `Clock: Copy` (both time types are `Copy`).
#[derive(Debug, Clone, Copy)]
pub struct Clock<'a> {
    global: TimelineTime,
    local: LocalTime,
    triggers: &'a TriggerTable,
    /// Resolved LOCAL window length (this component's own post-stretch seconds),
    /// or `None` for an open-ended placement (`.fill()`, a bare timeless point,
    /// the root). Carried for FRAME only — structure is never window-aware.
    window: Option<f64>,
}

impl<'a> Clock<'a> {
    /// Constructs a clock for one frame from both axes and the resolved table.
    pub fn new(global: TimelineTime, local: LocalTime, triggers: &'a TriggerTable) -> Self {
        Self {
            global,
            local,
            triggers,
            window: None,
        }
    }

    /// A neutral, time-zero clock over a shared empty [`TriggerTable`].
    ///
    /// Used by the `#[component(timeline)]` macro's clock-less delegators
    /// (`duration`/`measure`/`resolve`/`cues`/`arrangement`) to build the body
    /// when there is no per-frame clock to forward. This is sound because a
    /// component's STRUCTURE must be clock-independent by design (the audit
    /// model: `frame`/`render_audio_block` bake sampled values into a stable structure,
    /// so the resolved shape never varies with the clock value). A body that
    /// branches its structure on `clock` violates that contract.
    pub fn structural() -> Clock<'static> {
        static EMPTY: OnceLock<TriggerTable> = OnceLock::new();
        let triggers = EMPTY.get_or_init(TriggerTable::new);
        Clock {
            global: TimelineTime::new(0.0),
            local: LocalTime::new(0.0),
            triggers,
            window: None,
        }
    }

    /// 0 at THIS component's resolved start; survives `Sequence` re-flow.
    /// Self-animation: `clock.local().phase(0.0, 0.4)`.
    pub fn local(&self) -> LocalTime {
        self.local
    }

    /// Pure rebase: shifts the child's local axis but PRESERVES the window (used
    /// where the rebase does not change which window the child lives in).
    pub fn with_local(&self, local: LocalTime) -> Clock<'a> {
        Clock {
            global: self.global,
            local,
            triggers: self.triggers,
            window: self.window,
        }
    }

    /// Rebase AND set the child's resolved local window length in one step. The
    /// soundness rule (`.sketch/02 §8`): the window is set ONLY by the node that
    /// owns it (a `Placed` / `Sequence` slot) at the same site it rebases, never
    /// carried-then-cleared. A pure-rebase node uses [`with_local`](Self::with_local) instead.
    pub fn with_local_window(&self, local: LocalTime, window: Option<f64>) -> Clock<'a> {
        Clock {
            global: self.global,
            local,
            triggers: self.triggers,
            window,
        }
    }

    /// Absolute frame time — the SAME axis as [`Event`] triggers.
    pub fn global(&self) -> TimelineTime {
        self.global
    }

    /// Shifts BOTH time axes by `dt` seconds, evaluating the subtree as it
    /// was (or will be) `dt` away from this frame. The window length and the
    /// trigger table are untouched: the component still lives in the same
    /// resolved slot, only the cursor moves.
    ///
    /// This is the sampling primitive for temporal effects: a motion-blur
    /// shutter evaluates its child at several `shifted(-dt)` clocks and
    /// averages the frames. Shifting `global` together with `local` keeps
    /// [`Event`]-driven animation consistent with local-phase animation
    /// under the shifted clock.
    pub fn shifted(&self, dt: f64) -> Clock<'a> {
        Clock {
            global: TimelineTime::new(self.global.seconds() + dt),
            local: LocalTime::new(self.local.seconds() + dt),
            triggers: self.triggers,
            window: self.window,
        }
    }

    /// The resolved LOCAL window as a [`Window`] over the local axis —
    /// `[0, length)` with the cursor at [`local`](Self::local) — or `None`
    /// for an open-ended placement (`.fill()`, a bare timeless point, the
    /// root). End-relative effects read this and use the Window's own
    /// vocabulary: [`Window::remaining`] for countdowns,
    /// [`Window::envelope`] for fades, [`Window::phase`] for progress
    /// through the slot. For an open-ended fade-in, ease the local axis
    /// directly: `clock.local().phase(0.0, 0.4)`.
    pub fn window(&self) -> Option<Window> {
        self.window
            .map(|len| Window::new(0.0, len, self.local.seconds()))
    }

    /// Resolved trigger time of `e`, or `+∞` if unfired. Used by [`Event`]'s
    /// queries; not called directly by authors.
    pub(crate) fn trigger_of(&self, e: Event) -> TimelineTime {
        self.triggers.get(e.id())
    }
}
