//! Editing text with a caret: where typing goes, what a finger selects, and
//! which lines of a long entry are on the panel.
//!
//! The model behind a text field, kept apart from any keys, panel or socket so
//! that the runtime on a reader, the simulator on a desk and a test all edit
//! the same way. [`TextEdit`] is the text, its caret and its selection.
//! [`TextView`] breaks that text into lines at a width and keeps a window of
//! them on the panel. [`EditGestures`] turns touches in the field into caret
//! placement and selection. The caller measures and draws; nothing here knows
//! a typeface.
//!
//! ## Offsets
//!
//! Every offset is a byte offset into the text, and every offset this module
//! hands back is a grapheme cluster boundary, so an accented letter written
//! as two code points, or a flag, is stepped over, deleted and selected as the
//! one character a reader sees.
//!
//! ## Cost
//!
//! A field is laid out again on every keystroke, and a long one holds several
//! thousand characters. The lines are therefore kept between keystrokes and
//! only those from the edit onwards are broken again, stopping as soon as a
//! line starts where it started before the edit: from there on the text is
//! the same, so its lines are too. Typing in the middle of a long paragraph
//! costs a line or two of measuring, not the paragraph.

use crate::keyboard::MAX_TEXT;
use crate::select::{Touch, DOUBLE_TAP_MILLIS, DOUBLE_TAP_SLIP};
use std::ops::Range;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// How far, in logical pixels, a finger may move from where it pressed in a
/// field's text and still be tapping. Beyond it, the press selects.
///
/// Inside a field there is nothing else a drag could mean, so no hold time is
/// needed first; this is only there so a fingertip that rolls as it lifts
/// places a caret rather than selecting the letter it rolled over.
pub const DRAG_SLIP: i32 = 20;

/// The part of the text changed since the lines were last broken: in the old
/// text `from..old_to`, which is now `from..new_to`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Dirty {
    from: usize,
    old_to: usize,
    new_to: usize,
}

/// Text with a caret, and perhaps a selection, held to a length.
#[derive(Clone, Debug)]
pub struct TextEdit {
    text: String,
    /// Characters in `text`, kept rather than counted on every keystroke.
    chars: usize,
    caret: usize,
    /// The other end of the selection. Equal to `caret` when nothing is
    /// selected.
    anchor: usize,
    max_chars: usize,
    max_bytes: usize,
    dirty: Option<Dirty>,
}

impl Default for TextEdit {
    /// Empty, held to [`MAX_TEXT`] characters.
    fn default() -> Self {
        Self::new(MAX_TEXT, 4 * MAX_TEXT)
    }
}

/// Two edits are equal when they hold the same text, caret, selection and
/// limits, whatever lines a view still has to break again.
impl PartialEq for TextEdit {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.caret == other.caret
            && self.anchor == other.anchor
            && self.max_chars == other.max_chars
            && self.max_bytes == other.max_bytes
    }
}

impl Eq for TextEdit {}

impl TextEdit {
    /// Empty, holding at most `max_chars` characters in at most `max_bytes`
    /// bytes.
    #[must_use]
    pub const fn new(max_chars: usize, max_bytes: usize) -> Self {
        Self {
            text: String::new(),
            chars: 0,
            caret: 0,
            anchor: 0,
            max_chars,
            max_bytes,
            dirty: None,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many characters the text holds.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.chars
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The most characters the text may hold.
    #[must_use]
    pub const fn max_chars(&self) -> usize {
        self.max_chars
    }

    /// The most bytes the text may take.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Whether `text` would fit whole, in place of everything.
    #[must_use]
    pub fn fits(&self, text: &str) -> bool {
        text.len() <= self.max_bytes && text.chars().count() <= self.max_chars
    }

    /// Where typing goes. With a selection, the end the finger moved.
    #[must_use]
    pub const fn caret(&self) -> usize {
        self.caret
    }

    /// The selected text, start first, if anything is selected.
    #[must_use]
    pub fn selection(&self) -> Option<Range<usize>> {
        (self.anchor != self.caret)
            .then(|| self.anchor.min(self.caret)..self.anchor.max(self.caret))
    }

    /// Types `text` at the caret, in place of the selection if there is one.
    /// What would go past the limits is dropped, a whole character at a time.
    /// True if anything changed.
    pub fn insert(&mut self, text: &str) -> bool {
        let at = self.selection().unwrap_or(self.caret..self.caret);
        let removed = self.text[at.clone()].chars().count();
        let mut chars = self.chars - removed;
        let mut bytes = self.text.len() - at.len();
        let mut end = 0;
        for character in text.chars() {
            if chars + 1 > self.max_chars || bytes + character.len_utf8() > self.max_bytes {
                break;
            }
            chars += 1;
            bytes += character.len_utf8();
            end += character.len_utf8();
        }
        if at.is_empty() && end == 0 {
            return false;
        }
        self.replace(at, &text[..end]);
        true
    }

    /// Deletes the selection, or else the character before the caret. True if
    /// anything changed.
    pub fn backspace(&mut self) -> bool {
        let range = match self.selection() {
            Some(range) => range,
            None => match self.before(self.caret) {
                Some(start) => start..self.caret,
                None => return false,
            },
        };
        self.replace(range, "");
        true
    }

    /// Moves the caret one character towards the start, or to the start of
    /// the selection, dropping it. True if anything changed.
    pub fn left(&mut self) -> bool {
        let to = match self.selection() {
            Some(range) => range.start,
            None => match self.before(self.caret) {
                Some(to) => to,
                None => return false,
            },
        };
        self.place(to);
        true
    }

    /// Moves the caret one character towards the end, or to the end of the
    /// selection, dropping it. True if anything changed.
    pub fn right(&mut self) -> bool {
        let to = match self.selection() {
            Some(range) => range.end,
            None => match self.after(self.caret) {
                Some(to) => to,
                None => return false,
            },
        };
        self.place(to);
        true
    }

    /// Puts the caret at `offset`, or the character boundary before it, and
    /// drops any selection. True if anything changed.
    pub fn place(&mut self, offset: usize) -> bool {
        let offset = self.snap(offset);
        self.set(offset, offset)
    }

    /// Selects from `anchor` to `head`, each moved to the character boundary
    /// at or before it. The caret is at `head`. Equal ends select nothing and
    /// place the caret there. True if anything changed.
    pub fn select(&mut self, anchor: usize, head: usize) -> bool {
        let anchor = self.snap(anchor);
        let head = self.snap(head);
        self.set(anchor, head)
    }

    /// Selects the word at `offset`, or the word ending there; with no word
    /// at either, places the caret. True if anything changed.
    pub fn select_word(&mut self, offset: usize) -> bool {
        let offset = self.snap(offset);
        let mut found = None;
        for (start, piece) in self.text.split_word_bound_indices() {
            if start > offset {
                break;
            }
            if !piece.chars().any(char::is_alphanumeric) {
                continue;
            }
            let end = start + piece.len();
            if offset < end {
                found = Some(start..end);
                break;
            }
            if end == offset {
                found = Some(start..end);
            }
        }
        match found {
            Some(word) => self.set(word.start, word.end),
            None => self.place(offset),
        }
    }

    /// Drops the selection, leaving the caret where it is. True if there was
    /// one.
    pub fn deselect(&mut self) -> bool {
        self.set(self.caret, self.caret)
    }

    /// Empties the text.
    pub fn clear(&mut self) {
        self.take();
    }

    /// Takes the text, leaving this empty.
    pub fn take(&mut self) -> String {
        let length = self.text.len();
        self.note(0, length, 0);
        self.chars = 0;
        self.caret = 0;
        self.anchor = 0;
        std::mem::take(&mut self.text)
    }

    fn set(&mut self, anchor: usize, caret: usize) -> bool {
        let changed = (anchor, caret) != (self.anchor, self.caret);
        self.anchor = anchor;
        self.caret = caret;
        changed
    }

    fn replace(&mut self, range: Range<usize>, with: &str) {
        let removed = self.text[range.clone()].chars().count();
        self.text.replace_range(range.clone(), with);
        self.chars = self.chars - removed + with.chars().count();
        let caret = range.start + with.len();
        self.caret = caret;
        self.anchor = caret;
        self.note(range.start, range.end, caret);
    }

    /// Records that `from..to` of the text as it was is now `from..new_to`,
    /// on top of whatever was recorded already.
    fn note(&mut self, from: usize, to: usize, new_to: usize) {
        self.dirty = Some(match self.dirty {
            None => Dirty {
                from,
                old_to: to,
                new_to,
            },
            Some(earlier) => {
                // Where an offset of the text before this edit was, before
                // the earlier edits, and where it is after this one.
                let original = |offset: usize| {
                    if offset <= earlier.from {
                        offset
                    } else if offset >= earlier.new_to {
                        offset - earlier.new_to + earlier.old_to
                    } else {
                        earlier.old_to
                    }
                };
                let moved = |offset: usize| {
                    if offset <= from {
                        offset
                    } else if offset >= to {
                        offset - to + new_to
                    } else {
                        new_to
                    }
                };
                Dirty {
                    from: earlier.from.min(from),
                    old_to: earlier.old_to.max(original(to)),
                    new_to: moved(earlier.new_to).max(new_to),
                }
            }
        });
    }

    fn take_dirty(&mut self) -> Option<Dirty> {
        self.dirty.take()
    }

    /// `offset`, within the text and moved back to a character boundary.
    fn snap(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let mut cursor = GraphemeCursor::new(offset, self.text.len(), true);
        if cursor.is_boundary(&self.text, 0).unwrap_or(true) {
            offset
        } else {
            cursor
                .prev_boundary(&self.text, 0)
                .ok()
                .flatten()
                .unwrap_or(0)
        }
    }

    fn before(&self, offset: usize) -> Option<usize> {
        GraphemeCursor::new(offset, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
    }

    fn after(&self, offset: usize) -> Option<usize> {
        GraphemeCursor::new(offset, self.text.len(), true)
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
    }
}

/// A [`TextEdit`]'s text broken into lines at a width, and the window of
/// those lines that is on the panel.
///
/// Lines break after the whitespace between words; a word wider than the
/// line is broken between characters. Whitespace at the end of a line hangs
/// past it and is not drawn, except on the last line, where the caret may
/// sit after it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextView {
    width: i32,
    /// Where each line starts. Never empty once wrapped: empty text is one
    /// empty line.
    starts: Vec<usize>,
    /// The length of the text the lines were broken from.
    length: usize,
    top: usize,
    rows: usize,
}

impl TextView {
    /// Breaks `edit`'s text into lines no wider than `width` as `measure`
    /// measures them, again from wherever it changed since the last call.
    pub fn wrap(&mut self, edit: &mut TextEdit, width: i32, measure: &dyn Fn(&str) -> i32) {
        let dirty = edit.take_dirty();
        let text = edit.text();
        let width = width.max(1);
        let fresh = width != self.width || self.starts.is_empty();
        if !fresh && dirty.is_none() && self.length == text.len() {
            return;
        }
        let stale = dirty.is_none_or(|dirty| {
            self.length.checked_sub(dirty.old_to) != text.len().checked_sub(dirty.new_to)
        });
        if fresh || stale {
            self.wrap_all(edit, width, measure);
            return;
        }
        let Some(dirty) = dirty else { return };
        // The line before the one holding the start of the edited word may
        // now take that word back, so lines are broken again from there.
        let word = text[..dirty.from]
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(at, character)| at + character.len_utf8());
        let from = self.line_of(word).saturating_sub(1);
        let tail = self.starts.split_off(from + 1);
        self.length = text.len();
        self.continue_from(text, from, Some((dirty, &tail)), measure);
    }

    /// Breaks the whole of `edit`'s text into lines no wider than `width`,
    /// for a caller that cannot hand over the edit to be changed. Edits made
    /// before this are still picked up by the next [`Self::wrap`], which then
    /// does no more than it must.
    pub fn wrap_all(&mut self, edit: &TextEdit, width: i32, measure: &dyn Fn(&str) -> i32) {
        let text = edit.text();
        self.width = width.max(1);
        self.length = text.len();
        self.starts = vec![0];
        self.continue_from(text, 0, None, measure);
    }

    /// The width the lines were last broken at; 0 before they ever were.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.width
    }

    /// Breaks lines after line `from`, which is already in place. With the
    /// edit and the starts that followed it before, stops as soon as a line
    /// starts where one did before and takes the rest from there.
    fn continue_from(
        &mut self,
        text: &str,
        from: usize,
        resume: Option<(Dirty, &[usize])>,
        measure: &dyn Fn(&str) -> i32,
    ) {
        let mut start = self.starts[from];
        loop {
            let next = line_end(text, start, self.width, measure);
            if next >= text.len() {
                return;
            }
            if let Some((dirty, tail)) = resume {
                if next >= dirty.new_to {
                    let before = next - dirty.new_to + dirty.old_to;
                    if let Ok(at) = tail.binary_search(&before) {
                        self.starts.extend(
                            tail[at..]
                                .iter()
                                .map(|start| start - dirty.old_to + dirty.new_to),
                        );
                        return;
                    }
                }
            }
            self.starts.push(next);
            start = next;
        }
    }

    /// How many lines the text breaks into.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.starts.len().max(1)
    }

    /// The text of line `index`, from its start to where the next begins.
    #[must_use]
    pub fn line(&self, edit: &TextEdit, index: usize) -> Range<usize> {
        let start = self.starts.get(index).copied().unwrap_or(0);
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(edit.text().len());
        start..end.max(start)
    }

    /// Where the drawn text of line `index` ends: before the whitespace that
    /// hangs off its end, except on the last line.
    #[must_use]
    pub fn drawn_end(&self, edit: &TextEdit, index: usize) -> usize {
        let range = self.line(edit, index);
        if index + 1 >= self.lines() {
            return range.end;
        }
        range.start + edit.text()[range].trim_end().len()
    }

    /// The line `offset` is drawn on. An offset where one line ends and the
    /// next begins is drawn at the start of the next.
    #[must_use]
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    }

    /// The first line on the panel.
    #[must_use]
    pub const fn top(&self) -> usize {
        self.top
    }

    /// How many lines are on the panel.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// The lines on the panel.
    #[must_use]
    pub fn visible(&self) -> Range<usize> {
        self.top..(self.top + self.rows).min(self.lines())
    }

    /// Shows `rows` lines at most. With `follow`, the window moves as little
    /// as it can to show the line `edit`'s caret is on; without, it stays
    /// where it was paged to.
    pub fn show(&mut self, edit: &TextEdit, rows: usize, follow: bool) {
        self.rows = rows.clamp(1, self.lines());
        if follow {
            let line = self.line_of(edit.caret());
            if line < self.top {
                self.top = line;
            } else if line >= self.top + self.rows {
                self.top = line + 1 - self.rows;
            }
        }
        self.top = self.top.min(self.lines() - self.rows);
    }

    /// Moves the window a page, keeping one line of the page before in view.
    /// True if it moved.
    pub fn page(&mut self, forward: bool) -> bool {
        let step = self.rows.saturating_sub(1).max(1);
        let last = self.lines().saturating_sub(self.rows);
        let top = if forward {
            (self.top + step).min(last)
        } else {
            self.top.saturating_sub(step)
        };
        let moved = top != self.top;
        self.top = top;
        moved
    }

    /// The character boundary on line `index` nearest to `x`, measured from
    /// where the line is drawn. Past the end of a line is its end.
    ///
    /// `caret_width` is how much wider the line is drawn with the caret in
    /// it, when the caret is on this line and nothing is selected.
    #[must_use]
    pub fn boundary_at(
        &self,
        edit: &TextEdit,
        index: usize,
        x: i32,
        caret_width: i32,
        measure: &dyn Fn(&str) -> i32,
    ) -> usize {
        let index = index.min(self.lines() - 1);
        let start = self.line(edit, index).start;
        let end = self.drawn_end(edit, index);
        let text = edit.text();
        let caret = (edit.selection().is_none() && self.line_of(edit.caret()) == index)
            .then(|| edit.caret());
        let drawn = |offset: usize| {
            let shift = caret
                .filter(|caret| *caret < offset)
                .map_or(0, |_| caret_width);
            measure(&text[start..offset]).saturating_add(shift)
        };
        let mut best = (start, x.saturating_sub(drawn(start)).saturating_abs());
        for (at, cluster) in text[start..end].grapheme_indices(true) {
            let offset = start + at + cluster.len();
            let distance = x.saturating_sub(drawn(offset)).saturating_abs();
            if distance < best.1 {
                best = (offset, distance);
            }
        }
        best.0
    }
}

/// Where the line that starts at `start` ends and the next starts, or the
/// length of the text if the rest fits.
fn line_end(text: &str, start: usize, width: i32, measure: &dyn Fn(&str) -> i32) -> usize {
    let mut fitted = None;
    let mut word = false;
    let mut overflowed = false;
    for (at, character) in text[start..].char_indices() {
        let at = start + at;
        if character.is_whitespace() {
            if word {
                if measure(&text[start..at]) > width {
                    overflowed = true;
                    break;
                }
                fitted = Some(at);
            }
            word = false;
        } else {
            word = true;
        }
    }
    if word && !overflowed && measure(&text[start..]) <= width {
        return text.len();
    }
    if let Some(end) = fitted {
        return text[end..]
            .char_indices()
            .find(|(_, character)| !character.is_whitespace())
            .map_or(text.len(), |(at, _)| end + at);
    }
    if !word && !overflowed {
        // Nothing but whitespace from here.
        return text.len();
    }
    // One word wider than the line: as many characters of it as fit, and
    // always at least one, so a narrow line still moves on.
    let mut end = None;
    for (at, cluster) in text[start..].grapheme_indices(true) {
        let through = start + at + cluster.len();
        if end.is_some() && measure(&text[start..through]) > width {
            break;
        }
        end = Some(through);
    }
    end.unwrap_or(text.len())
}

/// What a touch did to the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditStep {
    /// Not in the text: handle the touch as if this were not here.
    Pass,
    /// Taken, and nothing changed.
    Quiet,
    /// The caret or the selection moved. Draw the field again.
    Changed,
}

impl EditStep {
    const fn of(changed: bool) -> Self {
        if changed {
            Self::Changed
        } else {
            Self::Quiet
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Contact {
    #[default]
    Up,
    /// Down somewhere other than the text, until it lifts.
    Elsewhere,
    /// Down in the text, the caret placed where it landed.
    Pressed { x: i32, y: i32, at: usize },
    /// Dragged from `anchor`: selecting.
    Dragging { anchor: usize },
    /// The second tap of a double tap, which selected a word.
    Word,
}

/// The touches in a field's text, as caret placement and selection.
///
/// A tap places the caret at the nearest character boundary. A press dragged
/// further than [`DRAG_SLIP`] selects from where it landed to where the
/// finger is, by character. A second tap close to the first and within
/// [`DOUBLE_TAP_MILLIS`] selects the word. Coordinates are the screen's
/// logical ones, and `hit` says which offset of the text, as it is when
/// asked, a point is: with its last argument false, only for a point on the
/// drawn text; with it true, for any point, as the nearest line would take
/// it, so a drag past the edge of the text keeps selecting.
#[derive(Clone, Debug, Default)]
pub struct EditGestures {
    contact: Contact,
    /// The last tap in the text: when it lifted, and where.
    tapped: Option<(u64, i32, i32)>,
}

impl EditGestures {
    /// Whether a finger pressed in the text is still down, so a move matters.
    #[must_use]
    pub const fn is_pressed(&self) -> bool {
        matches!(
            self.contact,
            Contact::Pressed { .. } | Contact::Dragging { .. } | Contact::Word
        )
    }

    /// Applies one touch, at `millis` on the caller's clock.
    pub fn touch(
        &mut self,
        touch: Touch,
        millis: u64,
        edit: &mut TextEdit,
        hit: &mut dyn FnMut(&TextEdit, i32, i32, bool) -> Option<usize>,
    ) -> EditStep {
        match touch {
            Touch::Down { x, y } => {
                let Some(at) = hit(edit, x, y, false) else {
                    self.contact = Contact::Elsewhere;
                    return EditStep::Pass;
                };
                let second = self.tapped.take().is_some_and(|(when, tx, ty)| {
                    millis.saturating_sub(when) <= DOUBLE_TAP_MILLIS
                        && (x - tx).abs() <= DOUBLE_TAP_SLIP
                        && (y - ty).abs() <= DOUBLE_TAP_SLIP
                });
                if second {
                    self.contact = Contact::Word;
                    EditStep::of(edit.select_word(at))
                } else {
                    self.contact = Contact::Pressed { x, y, at };
                    EditStep::of(edit.place(at))
                }
            }
            Touch::Move { x, y } => match self.contact {
                Contact::Pressed { x: x0, y: y0, at } => {
                    if (x - x0).abs() <= DRAG_SLIP && (y - y0).abs() <= DRAG_SLIP {
                        return EditStep::Quiet;
                    }
                    self.contact = Contact::Dragging { anchor: at };
                    self.drag(at, x, y, edit, hit)
                }
                Contact::Dragging { anchor } => self.drag(anchor, x, y, edit, hit),
                Contact::Word => EditStep::Quiet,
                Contact::Up | Contact::Elsewhere => EditStep::Pass,
            },
            Touch::Up { x, y } => match std::mem::take(&mut self.contact) {
                Contact::Pressed { .. } => {
                    self.tapped = Some((millis, x, y));
                    EditStep::Quiet
                }
                Contact::Dragging { anchor } => self.drag(anchor, x, y, edit, hit),
                Contact::Word => EditStep::Quiet,
                Contact::Up | Contact::Elsewhere => EditStep::Pass,
            },
            Touch::Cancel => {
                self.tapped = None;
                match std::mem::take(&mut self.contact) {
                    Contact::Up | Contact::Elsewhere => EditStep::Pass,
                    _ => EditStep::Quiet,
                }
            }
        }
    }

    fn drag(
        &mut self,
        anchor: usize,
        x: i32,
        y: i32,
        edit: &mut TextEdit,
        hit: &mut dyn FnMut(&TextEdit, i32, i32, bool) -> Option<usize>,
    ) -> EditStep {
        self.tapped = None;
        match hit(edit, x, y, true) {
            Some(head) => EditStep::of(edit.select(anchor, head)),
            None => EditStep::Quiet,
        }
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
