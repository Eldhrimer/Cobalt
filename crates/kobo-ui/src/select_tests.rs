use super::*;
use crate::{Chrome, NodeId, Overlay, Screen, CLARA_BW_METRICS};

const HOLD: ActionId = ActionId(7);

fn word(x: i32, y: i32, width: i32, context: u64, start: u32, end: u32) -> (Rect, TextHit) {
    (
        Rect {
            x,
            y,
            width,
            height: 30,
        },
        TextHit {
            context,
            start,
            end,
        },
    )
}

/// Two lines of one paragraph, and one line of another below them.
fn page() -> Layout {
    Layout {
        content: Rect {
            x: 0,
            y: 50,
            width: 600,
            height: 700,
        },
        hold: Some(HOLD),
        text_select: true,
        text_hits: vec![
            word(10, 100, 50, 1, 0, 5),
            word(70, 100, 40, 1, 6, 10),
            word(120, 100, 40, 1, 11, 15),
            word(10, 130, 50, 1, 16, 20),
            word(70, 130, 40, 1, 21, 25),
            word(10, 200, 50, 2, 0, 4),
        ],
        ..Layout::default()
    }
}

fn rect(x: i32, y: i32, width: i32) -> Rect {
    Rect {
        x,
        y,
        width,
        height: 30,
    }
}

fn hit(context: u64, start: u32, end: u32) -> TextHit {
    TextHit {
        context,
        start,
        end,
    }
}

fn selectable() -> impl FnMut() -> Option<Layout> {
    || Some(page())
}

fn done(kind: TextGestureKind, hit: TextHit, invert: Vec<Rect>) -> Step {
    Step::Done {
        gesture: Some((HOLD, TextGesture { kind, hit })),
        invert,
    }
}

fn step(step: Step) -> Outcome {
    Outcome {
        replay: false,
        step,
    }
}

#[test]
fn a_down_always_passes_so_press_feedback_still_runs() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    assert_eq!(
        selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout),
        step(Step::Pass)
    );
    assert_eq!(selector.deadline(), Some(SELECT_HOLD_MILLIS));
}

#[test]
fn a_hold_inverts_the_word_while_the_finger_is_down_and_the_lift_sends_it() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 1000, &mut layout);
    assert_eq!(selector.expire(1499, &mut layout), step(Step::Pass));
    assert!(!selector.is_active());
    assert_eq!(
        selector.expire(1500, &mut layout),
        step(Step::Paint {
            invert: vec![rect(10, 100, 50)]
        })
    );
    assert!(selector.is_active());
    assert_eq!(selector.deadline(), None);
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 1700, &mut layout),
        step(done(
            TextGestureKind::Select,
            hit(1, 0, 5),
            vec![rect(10, 100, 50)]
        ))
    );
    assert!(!selector.is_active());
}

#[test]
fn a_drag_follows_the_finger_forwards_and_backwards_one_rectangle_per_line() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 80, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    // Forwards onto the next line: the rest of the first line is one
    // rectangle, the start of the second another.
    assert_eq!(
        selector.touch(Touch::Move { x: 25, y: 140 }, 600, &mut layout),
        step(Step::Paint {
            invert: vec![rect(70, 100, 40), rect(70, 100, 90), rect(10, 130, 50)]
        })
    );
    // Within the same word: nothing changes, nothing is drawn.
    assert_eq!(
        selector.touch(Touch::Move { x: 30, y: 145 }, 650, &mut layout),
        step(Step::Quiet)
    );
    // Backwards past the anchor.
    assert_eq!(
        selector.touch(Touch::Move { x: 20, y: 110 }, 700, &mut layout),
        step(Step::Paint {
            invert: vec![rect(70, 100, 90), rect(10, 130, 50), rect(10, 100, 100)]
        })
    );
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 800, &mut layout),
        step(done(
            TextGestureKind::Select,
            hit(1, 0, 10),
            vec![rect(10, 100, 100)]
        ))
    );
}

#[test]
fn a_selection_never_leaves_the_paragraph_it_began_in() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    // Onto the other paragraph's word: the head stops at the nearest word of
    // this one, the start of its second line.
    selector.touch(Touch::Move { x: 20, y: 210 }, 600, &mut layout);
    let Step::Done {
        gesture: Some((_, gesture)),
        ..
    } = selector
        .touch(Touch::Up { x: 20, y: 210 }, 700, &mut layout)
        .step
    else {
        panic!("a lift ends the selection");
    };
    assert_eq!(gesture.hit, hit(1, 0, 20));
}

#[test]
fn the_nearest_word_weighs_a_line_away_more_than_a_word_along() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    // Below the end of the second line and right of it: its last word,
    // although the first line's last word is nearer along. Counted pixel for
    // pixel the first line would have won.
    selector.touch(Touch::Move { x: 175, y: 165 }, 600, &mut layout);
    let Step::Done {
        gesture: Some((_, gesture)),
        ..
    } = selector
        .touch(Touch::Up { x: 175, y: 165 }, 700, &mut layout)
        .step
    else {
        panic!("a lift ends the selection");
    };
    assert_eq!(gesture.hit, hit(1, 0, 25));
}

#[test]
fn moving_before_the_hold_time_lets_the_contact_go_for_good() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    assert_eq!(
        selector.touch(Touch::Move { x: 30, y: 140 }, 100, &mut layout),
        step(Step::Pass),
        "within the slip"
    );
    assert_eq!(
        selector.touch(
            Touch::Move {
                x: 20 + SELECT_SLIP + 1,
                y: 110
            },
            200,
            &mut layout
        ),
        step(Step::Pass)
    );
    assert_eq!(selector.deadline(), None);
    assert_eq!(selector.expire(600, &mut layout), step(Step::Pass));
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 700, &mut layout),
        step(Step::Pass)
    );
    assert!(!selector.has_deferred());
}

#[test]
fn a_hold_off_every_word_is_the_ordinary_hold() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 400, y: 400 }, 0, &mut layout);
    assert_eq!(selector.expire(500, &mut layout), step(Step::Pass));
    assert!(!selector.is_active());
    assert_eq!(
        selector.touch(Touch::Move { x: 20, y: 110 }, 600, &mut layout),
        step(Step::Pass),
        "dead until it lifts, even over a word"
    );
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 700, &mut layout),
        step(Step::Pass)
    );
}

#[test]
fn a_hold_the_timer_missed_is_begun_by_the_lift_and_draws_nothing() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 650, &mut layout),
        step(done(TextGestureKind::Select, hit(1, 0, 5), Vec::new()))
    );
}

#[test]
fn a_move_after_a_missed_timer_begins_and_extends_in_one_paint() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    assert_eq!(
        selector.touch(Touch::Move { x: 80, y: 110 }, 600, &mut layout),
        step(Step::Paint {
            invert: vec![rect(10, 100, 100)]
        })
    );
}

#[test]
fn a_quick_tap_on_a_word_is_held_back_until_the_window_closes() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 110 }, 80, &mut layout),
        step(Step::Defer)
    );
    assert!(selector.has_deferred());
    assert_eq!(selector.deadline(), Some(80 + DOUBLE_TAP_MILLIS));
    assert_eq!(
        selector.expire(80 + DOUBLE_TAP_MILLIS - 1, &mut layout),
        step(Step::Pass)
    );
    assert_eq!(
        selector.expire(80 + DOUBLE_TAP_MILLIS, &mut layout),
        Outcome {
            replay: true,
            step: Step::Pass
        }
    );
    assert!(!selector.has_deferred());
    assert_eq!(selector.deadline(), None);
}

#[test]
fn a_quick_tap_off_every_word_is_never_delayed() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 400, y: 400 }, 0, &mut layout);
    assert_eq!(
        selector.touch(Touch::Up { x: 400, y: 400 }, 80, &mut layout),
        step(Step::Pass)
    );
    assert!(!selector.has_deferred());
}

#[test]
fn two_quick_taps_on_a_word_are_a_double_tap() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 80, y: 110 }, 0, &mut layout);
    selector.touch(Touch::Up { x: 80, y: 110 }, 60, &mut layout);
    assert_eq!(
        selector.touch(Touch::Down { x: 90, y: 115 }, 200, &mut layout),
        step(Step::Pass),
        "the held tap is the first half, not something to deliver"
    );
    assert_eq!(
        selector.touch(Touch::Up { x: 90, y: 115 }, 260, &mut layout),
        step(done(TextGestureKind::DoubleTap, hit(1, 6, 10), Vec::new()))
    );
    assert!(!selector.has_deferred());
}

#[test]
fn a_second_contact_too_late_or_too_far_away_delivers_the_first_tap() {
    for (millis, x) in [
        (60 + DOUBLE_TAP_MILLIS, 80),
        (200, 80 + DOUBLE_TAP_SLIP + 1),
    ] {
        let mut selector = TextSelector::default();
        let mut layout = selectable();
        selector.touch(Touch::Down { x: 80, y: 110 }, 0, &mut layout);
        selector.touch(Touch::Up { x: 80, y: 110 }, 60, &mut layout);
        assert_eq!(
            selector.touch(Touch::Down { x, y: 110 }, millis, &mut layout),
            Outcome {
                replay: true,
                step: Step::Pass
            }
        );
        assert!(!selector.has_deferred());
    }
}

#[test]
fn a_second_tap_off_every_word_is_an_ordinary_tap() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 120, y: 125 }, 0, &mut layout);
    selector.touch(Touch::Up { x: 120, y: 125 }, 60, &mut layout);
    selector.touch(Touch::Down { x: 180, y: 125 }, 200, &mut layout);
    assert_eq!(
        selector.touch(Touch::Up { x: 180, y: 125 }, 260, &mut layout),
        step(Step::Pass)
    );
}

#[test]
fn a_double_tap_and_hold_inverts_the_paragraph_and_ignores_movement() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 80, y: 110 }, 0, &mut layout);
    selector.touch(Touch::Up { x: 80, y: 110 }, 60, &mut layout);
    selector.touch(Touch::Down { x: 80, y: 110 }, 200, &mut layout);
    let paragraph = vec![rect(10, 100, 150), rect(10, 130, 100)];
    assert_eq!(
        selector.expire(700, &mut layout),
        step(Step::Paint {
            invert: paragraph.clone()
        })
    );
    assert_eq!(
        selector.touch(Touch::Move { x: 20, y: 140 }, 800, &mut layout),
        step(Step::Quiet)
    );
    assert_eq!(
        selector.touch(Touch::Up { x: 20, y: 140 }, 900, &mut layout),
        step(done(
            TextGestureKind::DoubleTapHold,
            hit(1, 6, 10),
            paragraph
        ))
    );
}

#[test]
fn a_cancel_ends_a_selection_without_sending_anything() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    assert_eq!(
        selector.touch(Touch::Cancel, 600, &mut layout),
        step(Step::Done {
            gesture: None,
            invert: vec![rect(10, 100, 50)]
        })
    );
    assert_eq!(
        selector.touch(Touch::Cancel, 700, &mut layout),
        step(Step::Pass)
    );
}

#[test]
fn a_screen_that_is_not_selectable_passes_everything() {
    let mut nothing = || None;
    let mut not_opted = || {
        Some(Layout {
            text_select: false,
            ..page()
        })
    };
    for layout in [
        &mut nothing as &mut dyn FnMut() -> Option<Layout>,
        &mut not_opted,
    ] {
        let mut selector = TextSelector::default();
        selector.touch(Touch::Down { x: 20, y: 110 }, 0, layout);
        assert_eq!(
            selector.touch(Touch::Up { x: 20, y: 110 }, 60, layout),
            step(Step::Pass)
        );
        selector.touch(Touch::Down { x: 20, y: 110 }, 1000, layout);
        assert_eq!(selector.expire(1500, layout), step(Step::Pass));
        assert_eq!(
            selector.touch(Touch::Up { x: 20, y: 110 }, 1600, layout),
            step(Step::Pass)
        );
    }
}

#[test]
fn a_redrawn_surface_gets_the_whole_selection_drawn_again() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    selector.touch(Touch::Move { x: 80, y: 110 }, 600, &mut layout);
    assert_eq!(
        selector.repainted(&mut layout),
        Step::Paint {
            invert: vec![rect(10, 100, 100)]
        }
    );
    // What was drawn before the redraw is forgotten, not undone.
    assert_eq!(
        selector.touch(Touch::Up { x: 80, y: 110 }, 700, &mut layout),
        step(done(
            TextGestureKind::Select,
            hit(1, 0, 10),
            vec![rect(10, 100, 100)]
        ))
    );
}

#[test]
fn a_redrawn_surface_without_the_first_word_drops_the_selection_quietly() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.expire(500, &mut layout);
    let mut turned = || {
        Some(Layout {
            text_hits: vec![word(10, 100, 50, 3, 0, 5)],
            ..page()
        })
    };
    assert_eq!(selector.repainted(&mut turned), Step::Quiet);
    assert!(!selector.is_active());
    assert_eq!(
        selector.touch(Touch::Move { x: 80, y: 110 }, 600, &mut turned),
        step(Step::Quiet)
    );
    assert_eq!(
        selector.touch(Touch::Up { x: 80, y: 110 }, 700, &mut turned),
        step(Step::Quiet),
        "the lift is not taken for a tap"
    );
    assert_eq!(
        selector.touch(Touch::Down { x: 400, y: 400 }, 800, &mut turned),
        step(Step::Pass)
    );
}

#[test]
fn a_redraw_with_no_selection_up_asks_for_nothing() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    assert_eq!(selector.repainted(&mut layout), Step::Quiet);
}

#[test]
fn a_reset_forgets_the_contact_and_the_deferred_tap() {
    let mut selector = TextSelector::default();
    let mut layout = selectable();
    selector.touch(Touch::Down { x: 20, y: 110 }, 0, &mut layout);
    selector.touch(Touch::Up { x: 20, y: 110 }, 60, &mut layout);
    selector.reset();
    assert!(!selector.has_deferred());
    assert_eq!(selector.deadline(), None);
    assert_eq!(
        selector.touch(Touch::Down { x: 20, y: 110 }, 100, &mut layout),
        step(Step::Pass)
    );
}

#[test]
fn bounds_cover_every_rectangle() {
    assert_eq!(bounds(&[]), None);
    assert_eq!(
        bounds(&[rect(10, 100, 50), rect(70, 130, 40)]),
        Some(Rect {
            x: 10,
            y: 100,
            width: 100,
            height: 60
        })
    );
}

#[test]
fn gesture_kinds_have_fixed_wire_values() {
    for kind in [
        TextGestureKind::Select,
        TextGestureKind::DoubleTap,
        TextGestureKind::DoubleTapHold,
    ] {
        assert_eq!(TextGestureKind::from_wire(kind.wire_value()), Some(kind));
    }
    assert_eq!(TextGestureKind::Select.wire_value(), 0);
    assert_eq!(TextGestureKind::DoubleTap.wire_value(), 1);
    assert_eq!(TextGestureKind::DoubleTapHold.wire_value(), 2);
    assert_eq!(TextGestureKind::from_wire(3), None);
}

#[test]
fn a_screen_opts_in_and_an_overlay_turns_it_off() {
    let plain = Screen::new(1, Vec::new()).with_hold(HOLD);
    let layout = plain.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    assert_eq!(layout.hold, Some(HOLD));
    assert!(!layout.text_select, "a hold alone selects nothing");

    let selecting = Screen::new(1, Vec::new()).with_text_select(HOLD);
    assert_eq!(selecting.hold, Some(HOLD));
    assert!(selecting.text_select);
    let layout = selecting.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    assert!(layout.text_select);

    let covered = selecting.with_overlay(Overlay::modal(NodeId(9), "Covered", Vec::new()));
    let layout = covered.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    assert_eq!(layout.hold, None);
    assert!(!layout.text_select);
}
