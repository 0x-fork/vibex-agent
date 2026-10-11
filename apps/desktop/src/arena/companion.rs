//! The reward a won encounter unlocks: that Agent's guardian rides the strip
//! above its session composer while the turn runs, and dives into the input box
//! when the session stops. Double-clicking it opens the same duel the home page
//! offers, over the session page instead of the new-session page.

use super::{
    ArenaView, Awakened, art,
    combat::{Boss, Guardian, Phase, smoothstep},
    copy, motion, palette,
    raster::{PixelRect, ink},
    scene::{Geometry, PreviewSample, PreviewViewport},
};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, DismissEvent, Element,
    ElementId, Entity, EventEmitter, FocusHandle, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Style, Styled as _, Subscription, Window, div, fill, point,
    prelude::*, px, size,
};
use gpui_component::{ElementExt as _, InteractiveElementExt as _, WindowExt as _, v_flex};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

/// The strip a companion travels in, in screen pixels.
///
/// This is a lane above the input box, not a stage: the model is small enough
/// to read as a passenger of the composer rather than as the home page's
/// guardian, and the lane clips it so a dive vanishes at the input box's edge.
const LANE_HEIGHT: f32 = 42.0;
/// One full left-right pass.
const ROAM_PERIOD: f32 = 8.4;
/// The dive into the composer.
const LEAP_DURATION: f32 = 0.62;
/// Idle poses are rebuilt at this rate. The pixel art does not read faster, and
/// every rebuild rasterizes the whole sculpture.
const FRAME_RATE: f32 = 12.0;
/// The tallest screen pixels per world unit a companion may be drawn at.
const MAX_UNIT: f32 = 1.35;
/// The fraction of the lane's height a model may occupy.
const FILL: f32 = 0.86;
/// The clear margin kept at both ends of the travel.
const INSET: f32 = 12.0;

/// What the companion is doing, which is exactly what decides how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// No companion is mounted: no reward, no running turn, or a duel is up.
    Idle,
    /// The session is running; the companion travels its lane.
    Roaming { started: Instant },
    /// The turn ended; the companion is diving into the input box from where
    /// it stood, so the dive continues the pass instead of restarting it.
    Leaping { started: Instant, from: f32 },
}

/// The unlocked guardian that rides above one Agent session's composer.
pub(crate) struct Companion {
    guardian: Option<Guardian>,
    stage: Stage,
    /// Bumped whenever a stage replaces the one a task was scheduled for, so a
    /// retire task cannot retire a companion that has since started over.
    generation: u64,
    frames: Rc<RefCell<FrameCache>>,
    /// The lane's width, measured by its last prepaint. The travel is resolved
    /// while the tree is laid out, before this frame's bounds exist.
    lane_width: Rc<Cell<Pixels>>,
    /// How far into the current pass the companion stands, so a dive can start
    /// from the place the reader last saw it.
    roam: Rc<Cell<f32>>,
    /// The pose the companion last painted, handed to the duel it opens as the
    /// entry camera, so the model grows out of the lane it came from.
    sample: Rc<Cell<Option<PreviewSample>>>,
    /// Where the keyboard returns when a duel ends.
    composer_focus: Option<FocusHandle>,
    battle: Option<Entity<ArenaView>>,
    dismissal: Option<Subscription>,
}
impl EventEmitter<Awakened> for Companion {}
impl Companion {
    pub(crate) fn new() -> Self {
        Self {
            guardian: None,
            stage: Stage::Idle,
            generation: 0,
            frames: Rc::new(RefCell::new(FrameCache::default())),
            lane_width: Rc::new(Cell::new(px(0.0))),
            roam: Rc::new(Cell::new(0.0)),
            sample: Rc::new(Cell::new(None)),
            composer_focus: None,
            battle: None,
            dismissal: None,
        }
    }
    /// Follows the composer being rendered: which Agent it belongs to, whether
    /// its turn is running, and where the keyboard sits.
    ///
    /// This is called from the render path, so it never notifies. Everything it
    /// changes is readable by the same frame that asked for it.
    fn track(
        &mut self,
        reward: Option<&str>,
        running: bool,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        self.composer_focus = focus;
        let guardian = reward.and_then(Guardian::from_agent);
        if guardian != self.guardian {
            // A different Agent owns the composer now. The previous companion
            // leaves with the surface it belonged to: a dive would be a lie
            // about a session that is still running. A duel it opened belongs
            // to that session too, so it ends here rather than following the
            // reader to a session it was never about.
            self.guardian = guardian;
            self.generation += 1;
            self.frames.borrow_mut().clear();
            self.sample.set(None);
            self.roam.set(0.0);
            self.dismiss(cx);
            self.stage = match guardian {
                Some(_) if running => Stage::Roaming {
                    started: Instant::now(),
                },
                _ => Stage::Idle,
            };
            return;
        }
        match (running, self.stage) {
            (true, Stage::Roaming { .. }) | (false, Stage::Idle) => {}
            (false, Stage::Leaping { .. }) => {}
            (true, _) => {
                self.stage = Stage::Roaming {
                    started: Instant::now(),
                };
            }
            (false, Stage::Roaming { .. }) => self.leap(cx),
        }
    }
    /// Ends a duel without a result: the session it was opened over is gone.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let Some(battle) = self.battle.take() else {
            return;
        };
        self.dismissal = None;
        battle.update(cx, |view, cx| view.close(cx));
        cx.notify();
    }
    /// The session stopped: the companion dives into the input box and is gone
    /// by the time the dive lands.
    fn leap(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Leaping {
            started: Instant::now(),
            from: self.roam.get(),
        };
        self.generation += 1;
        let generation = self.generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor
                .timer(Duration::from_secs_f32(LEAP_DURATION) + Duration::from_millis(40))
                .await;
            this.update(cx, |companion, cx| {
                if companion.generation == generation {
                    companion.stage = Stage::Idle;
                    companion.sample.set(None);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.battle.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let Some(sample) = self.sample.get() else {
            return;
        };
        let battle = cx.new(|cx| ArenaView::new(sample, window, cx));
        let focus = self.composer_focus.clone();
        self.dismissal = Some(cx.subscribe_in(
            &battle,
            window,
            move |this, battle, _: &DismissEvent, window, cx| {
                if battle.read(cx).arena.phase == Phase::Victory
                    && let Some(guardian) = this.guardian
                {
                    // The duel was only reachable because this reward was
                    // already unlocked, so a replayed victory only ever
                    // re-reports the same fact.
                    cx.emit(Awakened {
                        key: guardian.key(),
                    });
                }
                this.battle = None;
                this.dismissal = None;
                this.sample.set(None);
                if let Some(focus) = focus.as_ref() {
                    focus.focus(window, cx);
                }
                cx.notify();
            },
        ));
        let initial_focus = battle.downgrade();
        self.battle = Some(battle);
        window.on_next_frame(move |window, cx| {
            let _ = initial_focus.update(cx, |view, cx| view.resume(window, cx));
        });
        cx.notify();
    }
    fn battle(&self) -> Option<Entity<ArenaView>> {
        self.battle.clone()
    }
}

/// Where a guardian's crop is drawn in the lane.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    /// Screen pixels per raster pixel.
    pixel: f32,
    width: f32,
    height: f32,
    /// The distance from the lane's top to the crop's top.
    top: f32,
}
fn fit(guardian: Guardian) -> Fit {
    let (left, top, right, bottom) = art::companion_box(guardian);
    let (box_width, box_height) = ((right - left) as f32, (bottom - top) as f32);
    // The model is fitted to the lane by height, so every guardian reads at
    // about the same size whatever its own proportions are.
    let unit = (LANE_HEIGHT * FILL * art::SCALE / box_height).min(MAX_UNIT);
    let pixel = unit / art::SCALE;
    let height = pixel * box_height;
    Fit {
        pixel,
        width: pixel * box_width,
        height,
        top: (LANE_HEIGHT - height) / 2.0,
    }
}
/// How far into a left-right pass the companion stands at `phase`, in screen
/// pixels from the lane's left edge.
fn roam_at(lane_width: f32, badge_width: f32, phase: f32) -> f32 {
    let span = (lane_width - badge_width - INSET * 2.0).max(0.0);
    // A triangle wave, eased at both ends: the companion walks out to one side,
    // turns around, and walks back.
    let wave = 1.0 - (phase.clamp(0.0, 1.0) * 2.0 - 1.0).abs();
    INSET + smoothstep(wave) * span
}

/// The lane above the composer, or nothing when this session has no reward to
/// show, when its turn is not running, or when motion is reduced.
pub(crate) fn lane(
    state: &Entity<Companion>,
    reward: Option<&str>,
    running: bool,
    focus: Option<FocusHandle>,
    reduced: bool,
    cx: &mut App,
) -> Option<AnyElement> {
    // Reduced motion drops the effect entirely — the setting asks for a still
    // workbench, and a roaming model is exactly the motion it turns off. The
    // companion is still told the turn ended, so nothing is left half-dived.
    state.update(cx, |companion, cx| {
        companion.track(reward, running && !reduced, focus, cx)
    });
    let (guardian, stage, frames, sample, lane_width, roam) = {
        let companion = state.read(cx);
        if reduced || companion.battle().is_some() {
            return None;
        }
        (
            companion.guardian?,
            companion.stage,
            companion.frames.clone(),
            companion.sample.clone(),
            companion.lane_width.clone(),
            companion.roam.clone(),
        )
    };
    if stage == Stage::Idle {
        return None;
    }
    let measured = lane_width.clone();
    let state = state.clone();
    let geometry = fit(guardian);
    let badge_width = geometry.width;
    let badge = move |time: f32, flipped: bool, opacity: f32, x: f32, y: f32| {
        div()
            .id("composer-companion")
            .debug_selector(|| "composer-companion".into())
            .absolute()
            .left(px(x))
            .top(px(y))
            .cursor_pointer()
            .aria_label(copy::companion_label(guardian))
            .on_double_click({
                let state = state.clone();
                move |_, window, cx| {
                    state.update(cx, |companion, cx| companion.open(window, cx));
                }
            })
            .child(Badge {
                guardian,
                time,
                flipped,
                opacity,
                pixel: px(geometry.pixel),
                window_box: art::companion_box(guardian),
                frames: frames.clone(),
                sample: sample.clone(),
            })
    };
    let lane = div()
        .id("composer-companion-lane")
        .debug_selector(|| "composer-companion-lane".into())
        .relative()
        .w_full()
        .min_w_0()
        .h(px(LANE_HEIGHT))
        .flex_none()
        .overflow_hidden()
        .on_prepaint(move |bounds, _, _| measured.set(bounds.size.width));
    match stage {
        Stage::Roaming { started } => {
            // The pose's own clock is monotonic: the idle loop is authored to
            // close on itself, so a pose never cuts back to its first frame in
            // the middle of the lane the way a per-pass clock would.
            let travelled = started.elapsed().as_secs_f32();
            let top = geometry.top;
            Some(
                lane.with_animation(
                    "composer-companion-roam",
                    Animation::new(Duration::from_secs_f32(ROAM_PERIOD))
                        .repeat()
                        .with_max_fps(20.0),
                    move |this, phase| {
                        roam.set(phase);
                        this.child(badge(
                            travelled,
                            phase >= 0.5,
                            1.0,
                            roam_at(f32::from(lane_width.get()), badge_width, phase),
                            top,
                        ))
                    },
                )
                .into_any_element(),
            )
        }
        Stage::Leaping { started, from } => {
            let top = geometry.top;
            Some(
                lane.with_animation(
                    "composer-companion-leap",
                    Animation::new(Duration::from_secs_f32(LEAP_DURATION)).with_max_fps(30.0),
                    move |this, delta| {
                        // Up a little, then down past the lane's own edge, where
                        // the input box's near side swallows it.
                        let hop = -(delta * std::f32::consts::PI).sin() * 9.0;
                        let dive = delta * delta * (LANE_HEIGHT + 18.0);
                        let opacity = 1.0 - smoothstep((delta - 0.55) / 0.45);
                        let x = roam_at(f32::from(lane_width.get()), badge_width, from);
                        this.child(badge(
                            started.elapsed().as_secs_f32(),
                            false,
                            opacity,
                            x,
                            top + hop + dive,
                        ))
                    },
                )
                .into_any_element(),
            )
        }
        Stage::Idle => None,
    }
}

/// Wraps a session page so an open duel covers it, exactly as it does on the
/// home page: the page fades away on entry and returns when the duel ends.
/// Without a duel the page is handed straight back, so the ordinary session
/// surface keeps the layout it always had.
pub(crate) fn session_surface(
    state: &Entity<Companion>,
    content: impl IntoElement,
    cx: &mut App,
) -> AnyElement {
    let Some(battle) = state.read(cx).battle() else {
        return content.into_any_element();
    };
    let opacity = battle.read(cx).home_opacity.clone();
    let page = div().w_full().min_w_0().min_h_0().flex_1().child(content);
    let content = if motion::reduced_motion(cx) {
        page.invisible().into_any_element()
    } else {
        page.with_animation(
            "composer-companion-veil",
            Animation::new(Duration::from_secs(60))
                .repeat()
                .with_max_fps(30.0),
            move |this, _| {
                let opacity = opacity.get();
                this.opacity(opacity)
                    .when(opacity <= 0.0, |this| this.invisible())
            },
        )
        .into_any_element()
    };
    v_flex()
        .id("composer-companion-surface")
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(content)
        .child(
            div()
                .id("composer-companion-duel")
                .debug_selector(|| "composer-companion-duel".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(battle),
        )
        .into_any_element()
}

/// Merged rectangles of the pose last drawn, so a repaint that lands on the
/// same pose costs nothing beyond the quads it paints.
#[derive(Default)]
struct FrameCache {
    key: Option<(Guardian, u64)>,
    rectangles: Vec<PixelRect>,
}
impl FrameCache {
    fn clear(&mut self) {
        self.key = None;
        self.rectangles.clear();
    }
    fn rectangles(&mut self, guardian: Guardian, time: f32) -> Vec<PixelRect> {
        let tick = (time * FRAME_RATE) as u64;
        if self.key != Some((guardian, tick)) {
            self.key = Some((guardian, tick));
            let time = tick as f32 / FRAME_RATE;
            let boss = Boss::idle(guardian, time, false);
            self.rectangles = art::companion(&boss, time, false).rectangles();
        }
        self.rectangles.clone()
    }
}

/// One cropped idle frame, painted as merged quads.
///
/// The element only carries the model's own rectangles; the box it was cropped
/// from is fixed for the whole loop, so the model never shifts inside it.
struct Badge {
    guardian: Guardian,
    time: f32,
    flipped: bool,
    opacity: f32,
    /// Screen pixels per raster pixel.
    pixel: Pixels,
    /// The crop window in raster pixels: `(left, top, right, bottom)`.
    window_box: (i32, i32, i32, i32),
    frames: Rc<RefCell<FrameCache>>,
    sample: Rc<Cell<Option<PreviewSample>>>,
}
impl IntoElement for Badge {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Badge {
    type RequestLayoutState = ();
    type PrepaintState = (Vec<PixelRect>, [Hsla; ink::COUNT]);
    fn id(&self) -> Option<ElementId> {
        Some("composer-companion-badge".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let (left, top, right, bottom) = self.window_box;
        let mut style = Style::default();
        style.size.width = (self.pixel * (right - left) as f32).into();
        style.size.height = (self.pixel * (bottom - top) as f32).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let rectangles = self
            .frames
            .borrow_mut()
            .rectangles(self.guardian, self.time);
        let (left, top, _, _) = self.window_box;
        let boss = Boss::idle(self.guardian, self.time, false);
        let (x, y) = art::pixel(boss.position);
        // The duel starts from this frame: the same model, in the same pose, at
        // the same spot, so it grows out of the lane it was standing in.
        self.sample.set(Some(PreviewSample {
            guardian: self.guardian,
            time: self.time,
            boss,
            viewport: Some(PreviewViewport {
                bounds,
                geometry: Geometry {
                    origin: bounds.origin
                        - point(
                            self.pixel * (x + left) as f32,
                            self.pixel * (y + top) as f32,
                        ),
                    unit: self.pixel * art::SCALE,
                },
            }),
        }));
        (rectangles, palette::material_colors(self.guardian))
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        (rectangles, colors): &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let pixel = self.pixel;
        let scale = window.scale_factor();
        let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
        let box_width = (self.window_box.2 - self.window_box.0) as f32;
        let clip = window.content_mask().bounds;
        for rect in rectangles.iter() {
            let x = if self.flipped {
                box_width - f32::from(rect.x + rect.width)
            } else {
                f32::from(rect.x)
            };
            let left = bounds.origin.x + pixel * x;
            let top = bounds.origin.y + pixel * f32::from(rect.y);
            let right = left + pixel * f32::from(rect.width);
            let bottom = top + pixel * f32::from(rect.height);
            let (left, top, right, bottom) = (snap(left), snap(top), snap(right), snap(bottom));
            if right > left
                && bottom > top
                && right >= clip.left()
                && left <= clip.right()
                && bottom >= clip.top()
                && top <= clip.bottom()
            {
                window.paint_quad(fill(
                    Bounds::new(point(left, top), size(right - left, bottom - top)),
                    colors[rect.ink as usize].opacity(self.opacity),
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "companion_tests.rs"]
mod tests;
