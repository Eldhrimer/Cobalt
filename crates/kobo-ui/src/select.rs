//! Selecting words with a finger: hold and drag, double tap, double tap and
//! hold.
//!
//! A screen opts in with [`crate::Screen::with_text_select`]. This module is
//! the whole of the decision about what a contact on such a screen means, in
//! the screen's logical coordinates and against a clock the caller supplies,
//! so the runtime on a reader and the simulator on a desk feed it the same
//! touches and get the same answers. It knows nothing about input devices,
//! panels or sockets: it says what to invert and what to send, and the caller
//! does both.
//!
//! The application decides what a sentence or a paragraph is. This names the
//! word, or the run of words, and the gesture that chose it.

use crate::{ActionId, Layout, Rect, TextHit};

/// How long a finger stays still on a word before a selection starts while it
/// is still down. The runtime's hold time; a test in `kobod` holds the two
/// equal.
pub const SELECT_HOLD_MILLIS: u64 = 500;
/// How far, in logical pixels, a finger may wander before the hold time and
/// still be holding.
pub const SELECT_SLIP: i32 = 40;
/// How soon after a quick tap on a word a second contact has to land to make
/// it a double tap.
pub const DOUBLE_TAP_MILLIS: u64 = 350;
/// How close to the first tap, in logical pixels, the second has to land.
pub const DOUBLE_TAP_SLIP: i32 = 90;

/// Which gesture chose the text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum TextGestureKind {
    /// Held, and perhaps dragged: the range runs from the held word to the
    /// word the finger lifted nearest.
    Select = 0,
    /// Two quick taps on one word. The application usually takes its
    /// sentence.
    DoubleTap = 1,
    /// A quick tap, then a second contact held still. The application
    /// usually takes its paragraph.
    DoubleTapHold = 2,
}

impl TextGestureKind {
    /// The byte this kind travels as.
    #[must_use]
    pub const fn wire_value(self) -> u8 {
        self as u8
    }

    /// The kind a byte names, if any.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Select),
            1 => Some(Self::DoubleTap),
            2 => Some(Self::DoubleTapHold),
            _ => None,
        }
    }
}

/// What a finger chose: the gesture, and the text in the application's own
/// coordinates. For [`TextGestureKind::Select`] the range may cover several
/// words of one context; for the other two it is the one word touched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextGesture {
    pub kind: TextGestureKind,
    pub hit: TextHit,
}

/// One touch, in logical coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Touch {
    Down {
        x: i32,
        y: i32,
    },
    Move {
        x: i32,
        y: i32,
    },
    Up {
        x: i32,
        y: i32,
    },
    /// The input stream lost part of the gesture.
    Cancel,
}

/// What the caller does with a touch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// Not the selector's: handle the touch exactly as without it.
    Pass,
    /// Swallowed, with nothing to draw.
    Quiet,
    /// An `Up` held back for the double-tap window. The caller keeps it and
    /// delivers it when an [`Outcome`] says `replay`.
    Defer,
    /// Invert these rectangles on the surface and refresh them. Old
    /// rectangles to undo and new ones to draw arrive in one list, so a
    /// change is painted once.
    Paint { invert: Vec<Rect> },
    /// The gesture is over. Send `gesture` when there is one; invert `invert`
    /// to put the surface back as the screen drew it.
    Done {
        gesture: Option<(ActionId, TextGesture)>,
        invert: Vec<Rect>,
    },
}

/// A [`Step`], and whether a deferred tap has to be delivered first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// Deliver the deferred `Up` as the ordinary tap it was before acting on
    /// `step`.
    pub replay: bool,
    pub step: Step,
}

impl Outcome {
    const fn step(step: Step) -> Self {
        Self {
            replay: false,
            step,
        }
    }
}

#[derive(Clone, Debug, Default)]
enum Contact {
    /// No finger down.
    #[default]
    Up,
    /// Down, and still for less than the hold time.
    Pending {
        at: u64,
        x: i32,
        y: i32,
        second: bool,
    },
    /// Not the selector's: everything passes until the finger lifts.
    Dead,
    /// The selection was dropped under the finger: everything is swallowed
    /// until it lifts, so the lift is not taken for a tap.
    Spent,
    Selecting(Selection),
}

#[derive(Clone, Debug)]
struct Selection {
    action: ActionId,
    anchor: TextHit,
    head: TextHit,
    /// The words of the anchor's context, as laid out when the selection
    /// began or the surface was last redrawn.
    hits: Vec<(Rect, TextHit)>,
    /// Begun by a second contact: the whole visible context is selected and
    /// movement is ignored.
    paragraph: bool,
}

impl Selection {
    fn range(&self) -> TextHit {
        TextHit {
            context: self.anchor.context,
            start: self.anchor.start.min(self.head.start),
            end: self.anchor.end.max(self.head.end),
        }
    }

    fn rects(&self) -> Vec<Rect> {
        let range = self.range();
        merge_lines(self.hits.iter().filter_map(|(rect, hit)| {
            (self.paragraph || (hit.start >= range.start && hit.end <= range.end)).then_some(*rect)
        }))
    }

    fn nearest(&self, x: i32, y: i32) -> Option<TextHit> {
        self.hits
            .iter()
            .min_by_key(|(rect, _)| {
                gap(y, rect.y, rect.height)
                    .saturating_mul(3)
                    .saturating_add(gap(x, rect.x, rect.width))
            })
            .map(|(_, hit)| *hit)
    }
}

/// The state machine. One per panel; see the module documentation.
#[derive(Clone, Debug, Default)]
pub struct TextSelector {
    contact: Contact,
    /// A quick tap on a word, held back: when its `Up` came, and where.
    deferred: Option<(u64, i32, i32)>,
    /// What is inverted on the surface now.
    drawn: Vec<Rect>,
}

impl TextSelector {
    /// Feeds one touch. `layout` returns the front screen's layout when that
    /// screen is selectable, and `None` otherwise; it is only called when the
    /// answer matters.
    pub fn touch(
        &mut self,
        touch: Touch,
        millis: u64,
        layout: &mut dyn FnMut() -> Option<Layout>,
    ) -> Outcome {
        match touch {
            Touch::Down { x, y } => {
                let deferred = self.deferred.take();
                let second = deferred.is_some_and(|(at, from_x, from_y)| {
                    millis.saturating_sub(at) < DOUBLE_TAP_MILLIS
                        && millis >= at
                        && (x - from_x).abs() <= DOUBLE_TAP_SLIP
                        && (y - from_y).abs() <= DOUBLE_TAP_SLIP
                });
                self.contact = Contact::Pending {
                    at: millis,
                    x,
                    y,
                    second,
                };
                Outcome {
                    replay: deferred.is_some() && !second,
                    step: Step::Pass,
                }
            }
            Touch::Move { x, y } => {
                if let Some(begun) = self.begin_if_due(millis, layout) {
                    let moved = self.moved(x, y);
                    return Outcome::step(merge_paints(begun, moved));
                }
                Outcome::step(match &self.contact {
                    Contact::Pending {
                        x: from_x,
                        y: from_y,
                        ..
                    } if (x - from_x).abs() > SELECT_SLIP || (y - from_y).abs() > SELECT_SLIP => {
                        self.contact = Contact::Dead;
                        Step::Pass
                    }
                    Contact::Selecting(_) => self.moved(x, y),
                    Contact::Spent => Step::Quiet,
                    Contact::Up | Contact::Pending { .. } | Contact::Dead => Step::Pass,
                })
            }
            Touch::Up { x, y } => {
                // A selection the timer never got to begin is begun here and
                // finished at once. What it would have drawn never reached
                // the surface, so only what was there before is undone.
                let before = self.drawn.clone();
                let begun = self.begin_if_due(millis, layout).is_some();
                if begun {
                    self.drawn = before;
                }
                let step = match std::mem::take(&mut self.contact) {
                    Contact::Selecting(selection) => {
                        let kind = if selection.paragraph {
                            TextGestureKind::DoubleTapHold
                        } else {
                            TextGestureKind::Select
                        };
                        let hit = if selection.paragraph {
                            selection.anchor
                        } else {
                            selection.range()
                        };
                        Step::Done {
                            gesture: Some((selection.action, TextGesture { kind, hit })),
                            invert: std::mem::take(&mut self.drawn),
                        }
                    }
                    Contact::Pending { second, .. } => self.quick_up(x, y, millis, second, layout),
                    Contact::Spent => Step::Quiet,
                    Contact::Up | Contact::Dead => Step::Pass,
                };
                Outcome::step(step)
            }
            Touch::Cancel => Outcome::step(match std::mem::take(&mut self.contact) {
                Contact::Selecting(_) => Step::Done {
                    gesture: None,
                    invert: std::mem::take(&mut self.drawn),
                },
                Contact::Spent => Step::Quiet,
                Contact::Up | Contact::Pending { .. } | Contact::Dead => Step::Pass,
            }),
        }
    }

    /// When [`TextSelector::expire`] next has something to do, on the
    /// caller's clock.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        match self.contact {
            Contact::Pending { at, .. } => Some(at.saturating_add(SELECT_HOLD_MILLIS)),
            Contact::Up => self
                .deferred
                .map(|(at, _, _)| at.saturating_add(DOUBLE_TAP_MILLIS)),
            Contact::Dead | Contact::Spent | Contact::Selecting(_) => None,
        }
    }

    /// Lets time pass: begins a selection whose hold time is up, and asks for
    /// a deferred tap whose window has closed with no finger down.
    pub fn expire(&mut self, millis: u64, layout: &mut dyn FnMut() -> Option<Layout>) -> Outcome {
        if let Some(step) = self.begin_if_due(millis, layout) {
            return Outcome::step(step);
        }
        if matches!(self.contact, Contact::Up)
            && self
                .deferred
                .is_some_and(|(at, _, _)| millis >= at.saturating_add(DOUBLE_TAP_MILLIS))
        {
            self.deferred = None;
            return Outcome {
                replay: true,
                step: Step::Pass,
            };
        }
        Outcome::step(Step::Pass)
    }

    /// The surface was redrawn under the selection, so nothing it inverted is
    /// there any more. Draws the selection again on the new surface when its
    /// first word is still on it, and otherwise lets it go without a word.
    pub fn repainted(&mut self, layout: &mut dyn FnMut() -> Option<Layout>) -> Step {
        let Contact::Selecting(selection) = &mut self.contact else {
            return Step::Quiet;
        };
        self.drawn.clear();
        let anchor = selection.anchor;
        let still = layout()
            .filter(|layout| layout.text_select && layout.hold == Some(selection.action))
            .map(|layout| context_hits(&layout, anchor.context))
            .filter(|hits| hits.iter().any(|(_, hit)| *hit == anchor));
        let Some(hits) = still else {
            self.contact = Contact::Spent;
            return Step::Quiet;
        };
        if !hits.iter().any(|(_, hit)| *hit == selection.head) {
            selection.head = anchor;
        }
        selection.hits = hits;
        let rects = selection.rects();
        self.drawn.clone_from(&rects);
        Step::Paint { invert: rects }
    }

    /// Forgets everything: the contact, the selection, what was drawn and any
    /// deferred tap. For when the surface or the application under it changed
    /// in a way the selector cannot follow.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether a selection is drawn, or being made, under a finger.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self.contact, Contact::Selecting(_))
    }

    /// Whether a quick tap is being held back for the double-tap window.
    #[must_use]
    pub const fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    fn begin_if_due(
        &mut self,
        millis: u64,
        layout: &mut dyn FnMut() -> Option<Layout>,
    ) -> Option<Step> {
        let Contact::Pending { at, x, y, second } = self.contact else {
            return None;
        };
        if millis < at.saturating_add(SELECT_HOLD_MILLIS) {
            return None;
        }
        let begun = layout().and_then(|layout| {
            let action = layout.hold.filter(|_| layout.text_select)?;
            let anchor = layout.hit_text(x, y)?;
            Some(Selection {
                action,
                anchor,
                head: anchor,
                hits: context_hits(&layout, anchor.context),
                paragraph: second,
            })
        });
        let Some(selection) = begun else {
            self.contact = Contact::Dead;
            return None;
        };
        let rects = selection.rects();
        let invert = difference(&self.drawn, &rects);
        self.drawn = rects;
        self.contact = Contact::Selecting(selection);
        Some(Step::Paint { invert })
    }

    fn moved(&mut self, x: i32, y: i32) -> Step {
        let Contact::Selecting(selection) = &mut self.contact else {
            return Step::Pass;
        };
        if selection.paragraph {
            return Step::Quiet;
        }
        let Some(head) = selection.nearest(x, y) else {
            return Step::Quiet;
        };
        selection.head = head;
        let rects = selection.rects();
        if rects == self.drawn {
            return Step::Quiet;
        }
        let invert = difference(&self.drawn, &rects);
        self.drawn = rects;
        Step::Paint { invert }
    }

    fn quick_up(
        &mut self,
        x: i32,
        y: i32,
        millis: u64,
        second: bool,
        layout: &mut dyn FnMut() -> Option<Layout>,
    ) -> Step {
        let Some(layout) = layout() else {
            return Step::Pass;
        };
        let Some(action) = layout.hold.filter(|_| layout.text_select) else {
            return Step::Pass;
        };
        let Some(hit) = layout.hit_text(x, y) else {
            return Step::Pass;
        };
        if second {
            return Step::Done {
                gesture: Some((
                    action,
                    TextGesture {
                        kind: TextGestureKind::DoubleTap,
                        hit,
                    },
                )),
                invert: std::mem::take(&mut self.drawn),
            };
        }
        self.deferred = Some((millis, x, y));
        Step::Defer
    }
}

/// The words of one context, in layout order.
fn context_hits(layout: &Layout, context: u64) -> Vec<(Rect, TextHit)> {
    layout
        .text_hits
        .iter()
        .filter(|(_, hit)| hit.context == context)
        .copied()
        .collect()
}

/// How far `value` is outside the span `from..from + length`, or zero inside
/// it.
fn gap(value: i32, from: i32, length: i32) -> i32 {
    let to = from.saturating_add(length.max(1)).saturating_sub(1);
    if value < from {
        from.saturating_sub(value)
    } else if value > to {
        value.saturating_sub(to)
    } else {
        0
    }
}

/// Words that share a line become one rectangle, from the first one's left to
/// the last one's right, so the gaps between them are inverted too.
fn merge_lines(rects: impl Iterator<Item = Rect>) -> Vec<Rect> {
    let mut lines: Vec<Rect> = Vec::new();
    for rect in rects {
        if let Some(line) = lines.iter_mut().find(|line| line.y == rect.y) {
            let left = line.x.min(rect.x);
            let right = (line.x.saturating_add(line.width)).max(rect.x.saturating_add(rect.width));
            let bottom =
                (line.y.saturating_add(line.height)).max(rect.y.saturating_add(rect.height));
            line.x = left;
            line.width = right.saturating_sub(left);
            line.height = bottom.saturating_sub(line.y);
        } else {
            lines.push(rect);
        }
    }
    lines
}

/// What to invert to turn `old` into `new`: every rectangle in one and not
/// the other. A rectangle in both is left alone.
fn difference(old: &[Rect], new: &[Rect]) -> Vec<Rect> {
    old.iter()
        .filter(|rect| !new.contains(rect))
        .chain(new.iter().filter(|rect| !old.contains(rect)))
        .copied()
        .collect()
}

/// A selection that began on the same touch that moved it is painted once.
fn merge_paints(first: Step, second: Step) -> Step {
    match (first, second) {
        (Step::Paint { invert: mut first }, Step::Paint { invert: second }) => {
            // Undoing and redoing the same rectangle is no change at all.
            for rect in second {
                if let Some(index) = first.iter().position(|drawn| *drawn == rect) {
                    first.remove(index);
                } else {
                    first.push(rect);
                }
            }
            Step::Paint { invert: first }
        }
        (first @ Step::Paint { .. }, _) => first,
        (_, second) => second,
    }
}

/// The bounding box of `rects`, if there are any.
#[must_use]
pub fn bounds(rects: &[Rect]) -> Option<Rect> {
    let first = *rects.first()?;
    let mut left = first.x;
    let mut top = first.y;
    let mut right = first.x.saturating_add(first.width);
    let mut bottom = first.y.saturating_add(first.height);
    for rect in &rects[1..] {
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.x.saturating_add(rect.width));
        bottom = bottom.max(rect.y.saturating_add(rect.height));
    }
    Some(Rect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    })
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
