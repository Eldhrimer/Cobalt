use super::*;
use std::cell::Cell;

/// Ten pixels a character, as a fixed-pitch face would set them.
fn mono(text: &str) -> i32 {
    i32::try_from(text.chars().count()).unwrap_or(i32::MAX) * 10
}

fn edit(text: &str) -> TextEdit {
    let mut edit = TextEdit::new(6000, 16_384);
    edit.insert(text);
    edit
}

fn lines(view: &TextView, edit: &TextEdit) -> Vec<String> {
    (0..view.lines())
        .map(|index| edit.text()[view.line(edit, index)].to_owned())
        .collect()
}

#[test]
fn typing_goes_in_at_the_caret_and_backspace_takes_the_character_before_it() {
    let mut edit = edit("helo world");
    assert_eq!(edit.caret(), 10);
    edit.place(3);
    assert!(edit.insert("l"));
    assert_eq!(edit.text(), "hello world");
    assert_eq!(edit.caret(), 4);
    assert!(edit.backspace());
    assert_eq!(edit.text(), "helo world");
    assert_eq!(edit.caret(), 3);
    edit.place(0);
    assert!(!edit.backspace(), "nothing before the start");
    assert_eq!(edit.count(), 10);
}

#[test]
fn a_selection_is_replaced_by_what_is_typed_and_removed_by_backspace() {
    let mut edit = edit("the quick brown fox");
    assert!(edit.select(4, 9));
    assert_eq!(edit.selection(), Some(4..9));
    assert!(edit.insert("slow"));
    assert_eq!(edit.text(), "the slow brown fox");
    assert_eq!(edit.selection(), None);
    assert_eq!(edit.caret(), 8);
    edit.select(18, 14);
    assert_eq!(
        edit.selection(),
        Some(14..18),
        "either end can be the anchor"
    );
    assert_eq!(edit.caret(), 14);
    assert!(edit.backspace());
    assert_eq!(edit.text(), "the slow brown");
    assert_eq!(edit.count(), 14);
}

#[test]
fn the_caret_keys_step_a_character_and_collapse_a_selection_to_its_end() {
    let mut edit = edit("abc");
    assert!(edit.left());
    assert!(edit.left());
    assert_eq!(edit.caret(), 1);
    assert!(edit.right());
    assert_eq!(edit.caret(), 2);
    edit.place(3);
    assert!(!edit.right(), "already at the end");
    edit.select(0, 2);
    assert!(edit.right());
    assert_eq!((edit.caret(), edit.selection()), (2, None));
    edit.select(1, 3);
    assert!(edit.left());
    assert_eq!((edit.caret(), edit.selection()), (1, None));
    edit.place(0);
    assert!(!edit.left());
}

#[test]
fn a_grapheme_cluster_is_one_character_to_the_caret_and_to_backspace() {
    // An e written with a combining acute, a flag, and a precomposed é: three
    // characters on the panel in eleven bytes.
    let mut edit = edit("e\u{301}\u{1F1EB}\u{1F1F7}\u{e9}");
    assert!(edit.left());
    assert_eq!(edit.caret(), 11);
    assert!(edit.left());
    assert_eq!(edit.caret(), 3);
    assert!(edit.left());
    assert_eq!(edit.caret(), 0);
    edit.place(2);
    assert_eq!(edit.caret(), 0, "the middle of a cluster snaps back");
    edit.place(5);
    assert_eq!(edit.caret(), 3, "the middle of a code point snaps back");
    edit.place(11);
    assert!(edit.backspace());
    assert_eq!(edit.text(), "e\u{301}\u{e9}");
    edit.place(3);
    assert!(edit.backspace());
    assert_eq!(edit.text(), "\u{e9}");
    assert_eq!(edit.count(), 1);
}

#[test]
fn multi_byte_text_counts_characters_and_is_held_to_both_limits() {
    let mut chars = TextEdit::new(4, 64);
    assert!(chars.insert("añoso"));
    assert_eq!(chars.text(), "años");
    assert_eq!(chars.count(), 4);
    assert!(!chars.insert("x"), "full");
    assert!(chars.fits("ñañá") && !chars.fits("ñañáñ"));

    let mut bytes = TextEdit::new(100, 5);
    assert!(bytes.insert("ééé"));
    assert_eq!(bytes.text(), "éé", "a third would need six bytes");
    bytes.select(0, 2);
    assert!(bytes.insert("abc"));
    assert_eq!(bytes.text(), "abcé");
    bytes.select(0, 4);
    assert!(bytes.insert("€€"), "the selection still goes");
    assert_eq!(bytes.text(), "€é");
}

#[test]
fn a_double_tap_takes_the_word_under_it_or_the_word_it_ends() {
    let mut edit = edit("Call me Ishmael. Some years ago");
    assert!(edit.select_word(10));
    assert_eq!(edit.selection(), Some(8..15));
    edit.select_word(15);
    assert_eq!(edit.selection(), Some(8..15), "just after the word");
    edit.select_word(0);
    assert_eq!(edit.selection(), Some(0..4));
    edit.select_word(16);
    assert_eq!(edit.selection(), None, "between the full stop and a space");
    assert_eq!(edit.caret(), 16);
    let mut accented = self::edit("un café noir");
    accented.select_word(5);
    assert_eq!(accented.selection(), Some(3..8));
}

#[test]
fn lines_break_after_spaces_and_a_word_too_wide_breaks_between_letters() {
    let mut edit = edit("the quick brown fox jumps");
    let mut view = TextView::default();
    view.wrap(&mut edit, 100, &mono);
    assert_eq!(lines(&view, &edit), ["the quick ", "brown fox ", "jumps"]);
    assert_eq!(view.drawn_end(&edit, 0), 9, "the space hangs off the line");
    assert_eq!(view.drawn_end(&edit, 2), 25);
    let mut long = self::edit("ab abcdefghijklmnop q");
    view.wrap(&mut long, 60, &mono);
    assert_eq!(lines(&view, &long), ["ab ", "abcdef", "ghijkl", "mnop q"]);
    let mut empty = TextEdit::default();
    view.wrap(&mut empty, 60, &mono);
    assert_eq!(view.lines(), 1);
    assert_eq!(view.line(&empty, 0), 0..0);
}

#[test]
fn lines_broken_again_after_an_edit_are_the_lines_of_the_edited_text() {
    let words = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
                 eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
    let mut edit = edit(&words.repeat(4));
    let mut view = TextView::default();
    view.wrap(&mut edit, 200, &mono);
    let edits: [(usize, usize, &str); 8] = [
        (0, 0, "X"),
        (40, 41, ""),
        (
            100,
            100,
            "a much longer insertion that spans a line or two of text ",
        ),
        (77, 120, ""),
        (5, 6, "_"),
        (200, 200, "supercalifragilisticexpialidocious"),
        (300, 301, " "),
        (10, 300, ""),
    ];
    for (start, end, with) in edits {
        edit.select(start, end);
        edit.insert(with);
        // A second edit before the lines are broken again.
        edit.place(3);
        edit.insert("é");
        view.wrap(&mut edit, 200, &mono);
        let mut fresh = TextView::default();
        fresh.wrap_all(&edit, 200, &mono);
        assert_eq!(
            lines(&view, &edit),
            lines(&fresh, &edit),
            "after {start}..{end}"
        );
    }
    // Lines broken whole while an edit was outstanding, then an edit: the
    // next wrap still comes out right.
    edit.place(50);
    edit.insert("zz ");
    view.wrap_all(&edit, 200, &mono);
    edit.place(120);
    edit.insert("q");
    view.wrap(&mut edit, 200, &mono);
    let mut fresh = TextView::default();
    fresh.wrap_all(&edit, 200, &mono);
    assert_eq!(lines(&view, &edit), lines(&fresh, &edit));
    assert_eq!(view.width(), 200);
}

#[test]
fn a_keystroke_in_the_middle_of_a_long_text_measures_only_the_lines_near_it() {
    let measured = Cell::new(0_usize);
    let counting = |text: &str| {
        measured.set(measured.get() + 1);
        mono(text)
    };
    let mut edit = edit(&"word ".repeat(1200));
    let mut view = TextView::default();
    view.wrap(&mut edit, 300, &counting);
    let whole = measured.replace(0);
    edit.place(3000);
    edit.insert("x");
    view.wrap(&mut edit, 300, &counting);
    let keystroke = measured.get();
    assert!(
        keystroke * 20 < whole,
        "a keystroke measured {keystroke} times, the whole text {whole}"
    );
}

#[test]
fn the_window_follows_the_caret_and_pages_keep_a_line_of_overlap() {
    let mut edit = edit(&"abcd ".repeat(20));
    let mut view = TextView::default();
    view.wrap(&mut edit, 50, &mono);
    assert_eq!(view.lines(), 20);
    view.show(&edit, 5, true);
    assert_eq!(view.visible(), 15..20, "the caret is at the end");
    edit.place(0);
    view.show(&edit, 5, true);
    assert_eq!(view.visible(), 0..5);
    assert!(view.page(true));
    assert_eq!(view.visible(), 4..9);
    view.show(&edit, 5, false);
    assert_eq!(view.visible(), 4..9, "a page is not undone by drawing");
    edit.place(60);
    view.show(&edit, 5, true);
    assert_eq!(view.visible(), 8..13);
    for _ in 0..10 {
        view.page(true);
    }
    assert_eq!(view.visible(), 15..20);
    assert!(!view.page(true));
    view.show(&edit, 50, true);
    assert_eq!(view.visible(), 0..20, "more room than lines");
    assert!(!view.page(false));
}

#[test]
fn a_point_on_a_line_snaps_to_the_nearer_side_of_the_character_under_it() {
    let mut edit = edit("the quick brown fox");
    let mut view = TextView::default();
    view.wrap(&mut edit, 100, &mono);
    edit.select(0, 1);
    assert_eq!(view.boundary_at(&edit, 0, 14, 4, &mono), 1);
    assert_eq!(view.boundary_at(&edit, 0, 16, 4, &mono), 2);
    assert_eq!(view.boundary_at(&edit, 0, -30, 4, &mono), 0);
    assert_eq!(
        view.boundary_at(&edit, 0, 400, 4, &mono),
        9,
        "past the end of a wrapped line is before its hanging space"
    );
    assert_eq!(view.boundary_at(&edit, 1, 400, 4, &mono), 19);
    // With the caret drawn after the second character, everything after it
    // is four pixels further on.
    edit.place(12);
    assert_eq!(view.boundary_at(&edit, 1, 24, 4, &mono), 12);
    assert_eq!(view.boundary_at(&edit, 1, 32, 4, &mono), 13);
    assert_eq!(
        view.boundary_at(&edit, 1, 27, 4, &mono),
        12,
        "without the caret, 27 would be nearer 13"
    );
}

/// One line of text, ten pixels a character, from (100, 200), thirty high.
fn hit(edit: &TextEdit, x: i32, y: i32, clamp: bool) -> Option<usize> {
    if !clamp && !(200..230).contains(&y) {
        return None;
    }
    let at = usize::try_from(((x - 100) + 5).max(0) / 10).unwrap_or(0);
    Some(at.min(edit.text().len()))
}

fn touch(gestures: &mut EditGestures, edit: &mut TextEdit, touch: Touch, millis: u64) -> EditStep {
    gestures.touch(touch, millis, edit, &mut hit)
}

#[test]
fn a_tap_places_the_caret_and_a_drag_selects_while_the_finger_is_down() {
    let mut edit = edit("hello world");
    let mut gestures = EditGestures::default();
    let step = touch(&mut gestures, &mut edit, Touch::Down { x: 128, y: 210 }, 0);
    assert_eq!(step, EditStep::Changed);
    assert_eq!(edit.caret(), 3);
    assert!(gestures.is_pressed());
    let wobble = touch(&mut gestures, &mut edit, Touch::Move { x: 135, y: 214 }, 10);
    assert_eq!(wobble, EditStep::Quiet, "a wobble is still a tap");
    assert_eq!(
        touch(&mut gestures, &mut edit, Touch::Up { x: 135, y: 214 }, 20),
        EditStep::Quiet
    );
    assert_eq!((edit.caret(), edit.selection()), (3, None));

    touch(
        &mut gestures,
        &mut edit,
        Touch::Down { x: 160, y: 210 },
        1000,
    );
    let dragged = touch(
        &mut gestures,
        &mut edit,
        Touch::Move { x: 200, y: 212 },
        1010,
    );
    assert_eq!(dragged, EditStep::Changed);
    assert_eq!(edit.selection(), Some(6..10));
    // Past the bottom of the text, the drag goes on selecting.
    touch(
        &mut gestures,
        &mut edit,
        Touch::Move { x: 220, y: 300 },
        1020,
    );
    assert_eq!(edit.selection(), Some(6..11));
    touch(
        &mut gestures,
        &mut edit,
        Touch::Move { x: 120, y: 210 },
        1030,
    );
    assert_eq!(edit.selection(), Some(2..6), "back past where it landed");
    assert_eq!(edit.caret(), 2);
    touch(&mut gestures, &mut edit, Touch::Up { x: 120, y: 210 }, 1040);
    assert_eq!(edit.selection(), Some(2..6), "kept when the finger lifts");
    assert!(!gestures.is_pressed());

    // A tap elsewhere in the text drops it.
    touch(
        &mut gestures,
        &mut edit,
        Touch::Down { x: 210, y: 210 },
        3000,
    );
    touch(&mut gestures, &mut edit, Touch::Up { x: 210, y: 210 }, 3010);
    assert_eq!((edit.caret(), edit.selection()), (11, None));
}

#[test]
fn a_double_tap_selects_a_word_and_a_slow_one_is_two_taps() {
    let mut edit = edit("hello world");
    let mut gestures = EditGestures::default();
    for (touch_, at) in [
        (Touch::Down { x: 170, y: 210 }, 0),
        (Touch::Up { x: 170, y: 210 }, 40),
        (Touch::Down { x: 172, y: 212 }, 200),
    ] {
        touch(&mut gestures, &mut edit, touch_, at);
    }
    assert_eq!(edit.selection(), Some(6..11));
    touch(
        &mut gestures,
        &mut edit,
        Touch::Move { x: 120, y: 212 },
        220,
    );
    assert_eq!(edit.selection(), Some(6..11), "a word is not dragged");
    touch(&mut gestures, &mut edit, Touch::Up { x: 120, y: 212 }, 240);

    let mut slow = self::edit("hello world");
    let mut gestures = EditGestures::default();
    touch(&mut gestures, &mut slow, Touch::Down { x: 170, y: 210 }, 0);
    touch(&mut gestures, &mut slow, Touch::Up { x: 170, y: 210 }, 40);
    touch(
        &mut gestures,
        &mut slow,
        Touch::Down { x: 170, y: 210 },
        40 + DOUBLE_TAP_MILLIS + 1,
    );
    assert_eq!((slow.caret(), slow.selection()), (7, None));
}

#[test]
fn a_touch_that_starts_outside_the_text_is_not_taken() {
    let mut edit = edit("hello");
    edit.select(0, 5);
    let mut gestures = EditGestures::default();
    for (step, at) in [
        (Touch::Down { x: 120, y: 600 }, 0),
        (Touch::Move { x: 120, y: 210 }, 10),
        (Touch::Up { x: 120, y: 210 }, 20),
    ] {
        assert_eq!(touch(&mut gestures, &mut edit, step, at), EditStep::Pass);
    }
    assert_eq!(edit.selection(), Some(0..5));
    assert_eq!(
        touch(&mut gestures, &mut edit, Touch::Cancel, 30),
        EditStep::Pass
    );
}
