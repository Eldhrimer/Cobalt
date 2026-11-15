//! The keyboard the runtime draws when an application asks for text.
//!
//! Shared by the device host and the simulator, so a script that types in the
//! simulator types on the same keys, laid out the same way, that the reader
//! will draw.
//!
//! ## What stays on the panel
//!
//! The field replaces the application's screen rather than sitting over it,
//! and keeps the application's title in the top bar. An application's screen
//! was laid out for the whole panel, so the keys can only sit under it by
//! cutting off whatever was at its foot, and at the largest text size on a
//! small panel turned on its side that is everything below the title. What a
//! reader needs while typing is which application is asking and what for: the
//! title says the first, and the prompt and its one line of guidance, drawn
//! here in the runtime's own type, say the second. Nothing the application
//! drew can be tapped while the field is up, so there is no control behind
//! the keys that could act on text the application has not been given yet.
//!
//! The way out is the runtime's own Back, in the top bar where it is on every
//! other screen. A Cancel under the keys as well would be a second control
//! meaning the same thing, and the row it took is the row the largest text
//! size needs.
//!
//! When even that does not fit, the guidance goes first, then the prompt moves
//! into the top bar in place of the title, and last only some of a long
//! entry is shown. The keys never shrink below a fingertip.
//!
//! ## The caret
//!
//! Every field but a secret is edited with a caret, through
//! [`kobo_ui::edit`]: it is drawn as a bar in the text, a tap in the text puts
//! it at the nearest character boundary, a press dragged across the text
//! selects, and a double tap selects a word. What is typed goes in at the
//! caret or in place of the selection. The entry is set as many lines as fit
//! above the keys; when it is longer than that, the window follows the caret
//! and two controls under it page through the rest.
//!
//! A selection is drawn in the wash and underline a reader's highlight is
//! drawn in, as part of the field, rather than inverted on the panel: the
//! field is drawn again on every change anyway, and the same screen is then
//! what the reader, the simulator and a capture all show.
//!
//! A secret is typed at its end, as it always was, with no caret drawn and no
//! touch in its text taken: nothing about where in it a finger went is shown
//! or kept.

use kobo_protocol::{
    DenyReason, DeviceError, DeviceRequest, DeviceResult, SecretTarget, TextOutcome,
};
use kobo_ui::edit::{EditGestures, EditStep, TextView};
use kobo_ui::keyboard::{self, Keyboard, Pressed};
use kobo_ui::select::Touch;
use kobo_ui::{
    ActionId, Cell, Chrome, DiagnosticSeverity, DisplayMetrics, Face, FieldKind, FontSize, Layout,
    LayoutKind, Node, NodeId, ParagraphPresentation, Rect, RichTextSpan, Screen, TextPresentation,
    TopBar,
};
use std::cell::RefCell;
use std::fmt;

/// The top bar control that shows a secret, or hides it again.
pub const REVEAL: &str = "kb.reveal";

/// The controls under a long entry that show its earlier and later lines.
pub const PAGE_UP: &str = "kb.page.up";
pub const PAGE_DOWN: &str = "kb.page.down";

/// The node an entry is drawn as, so a touch can be found in it.
const TEXT_NODE: NodeId = NodeId(0x6b62_0001);

/// How the caret is drawn: a bar between two characters.
const CARET: &str = "|";

/// The most lines of an entry ever drawn at once.
const MAX_ROWS: usize = 64;

/// The fewest lines worth a pair of page controls. With less room than that,
/// the window only follows the caret.
const MIN_PAGED_ROWS: usize = 2;

/// The fewest lines of a long entry worth keeping the prompt above them for.
/// With less room, the prompt moves into the top bar to make more.
const COMFORTABLE_ROWS: usize = 4;

/// The identity of the screen the runtime draws for a field, so a capture or
/// a log line says whose screen it was.
const SCREEN_NAME: &str = "cobalt.keyboard";

/// How many characters of a long entry are kept on the panel when the whole
/// of it does not fit above the keys.
const TAILS: [usize; 2] = [80, 24];

/// A text field the runtime is holding open for an application.
pub struct Field {
    prompt: String,
    hint: String,
    submit: String,
    save_as: Option<SecretTarget>,
    keyboard: Keyboard,
    revealed: bool,
    gestures: EditGestures,
    /// The entry's lines and how they were last fitted to the panel. Kept
    /// apart because drawing, which only reads the field, is when the panel
    /// is known.
    view: RefCell<View>,
}

/// An entry's lines, and the panel they were fitted to.
#[derive(Default)]
struct View {
    lines: TextView,
    fit: Option<Fit>,
    /// The caret moved or the text changed since the field was last drawn,
    /// so the window goes to the caret.
    follow: bool,
}

/// How an entry fits on one panel under one title.
#[derive(Clone, PartialEq)]
struct Fit {
    metrics: DisplayMetrics,
    title: Option<String>,
    /// The width of the entry's lines.
    width: i32,
    /// How much wider a line is with the caret in it.
    caret: i32,
    /// The most lines that fit in each [`Shape`], without and then with the
    /// page controls, found when first wanted.
    room: [[Option<usize>; 2]; 3],
}

/// What a touch did to an open field.
#[derive(Debug, Eq, PartialEq)]
pub enum Touched {
    /// Not the field's text: resolve it against the keys as a tap.
    Pass,
    /// Taken, and nothing to draw.
    Taken,
    /// The caret or the selection moved. Draw the field again.
    Redraw,
}

/// Written out by hand so that a field holding a password can be logged.
impl fmt::Debug for Field {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Field")
            .field("prompt", &self.prompt)
            .field("kind", &self.keyboard.kind())
            .field("characters", &self.keyboard.count())
            .finish_non_exhaustive()
    }
}

/// What a tap did to an open field.
#[derive(Debug, Eq, PartialEq)]
pub enum Step {
    /// The field or its keys changed. Draw it again.
    Redraw,
    /// The owner is done. Answer the application, and draw its screen again.
    Finished(Finished),
}

/// How the owner left a field.
#[derive(Eq, PartialEq)]
pub enum Finished {
    /// Text to hand to the application.
    Typed(String),
    /// A secret, as the request that stores it. It is answered as
    /// [`TextOutcome::Saved`] and never handed to the application.
    Secret(DeviceRequest),
    Cancelled,
}

impl fmt::Debug for Finished {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Typed(text) => formatter.debug_tuple("Typed").field(text).finish(),
            Self::Secret(_) => formatter.write_str("Secret"),
            Self::Cancelled => formatter.write_str("Cancelled"),
        }
    }
}

impl Field {
    /// Opens a field for `app`'s request.
    ///
    /// `None` when the request is not a text request. A secret that `app`
    /// may not keep under that name is refused here, before the owner types
    /// a word of it, with the answer to send instead.
    ///
    /// # Errors
    ///
    /// The [`DeviceResult`] that refuses the request.
    #[must_use]
    pub fn open(app: &str, request: &DeviceRequest) -> Option<Result<Self, DeviceResult>> {
        let DeviceRequest::TypeText {
            prompt,
            hint,
            initial,
            submit,
            kind,
            save_as,
        } = request
        else {
            return None;
        };
        if let Some(target) = save_as {
            // Asked with a stand-in value: the question is whether this
            // application may keep a secret under this name at all, and the
            // answer must come before the owner types the real one.
            if let Some(Err(refusal)) =
                kobo_policy::credentials::validate_install(app, &target.install("x"))
            {
                return Some(Err(DeviceResult::Text(match refusal {
                    DeviceResult::Denied(reason) => TextOutcome::Denied(reason),
                    DeviceResult::Failed(error) => TextOutcome::Failed(error),
                    _ => TextOutcome::Denied(DenyReason::PolicyRejected),
                })));
            }
        }
        Some(Ok(Self {
            prompt: prompt.clone(),
            hint: hint.clone(),
            submit: submit.clone(),
            save_as: save_as.clone(),
            keyboard: Keyboard::for_kind(*kind).typed(initial.as_str()),
            revealed: false,
            gestures: EditGestures::default(),
            view: RefCell::new(View {
                follow: true,
                ..View::default()
            }),
        }))
    }

    #[must_use]
    pub fn kind(&self) -> FieldKind {
        self.keyboard.kind()
    }

    /// Applies one tap on the field's screen.
    ///
    /// `None` for a tap that meant nothing here, which is answered by doing
    /// nothing: the application's controls are not on this screen, so there
    /// is nobody else it could have been for.
    pub fn press(&mut self, action: ActionId) -> Option<Step> {
        if action == ActionId::BACK {
            return Some(Step::Finished(Finished::Cancelled));
        }
        if action == ActionId::from_name(REVEAL) && self.kind() == FieldKind::Secret {
            self.revealed = !self.revealed;
            return Some(Step::Redraw);
        }
        if self.kind() != FieldKind::Secret {
            for (name, forward) in [(PAGE_UP, false), (PAGE_DOWN, true)] {
                if action == ActionId::from_name(name) {
                    return self
                        .view
                        .get_mut()
                        .lines
                        .page(forward)
                        .then_some(Step::Redraw);
                }
            }
        }
        let modifier = [keyboard::SHIFT, keyboard::LAYER]
            .iter()
            .any(|name| action == ActionId::from_name(name));
        Some(match self.keyboard.press(action)? {
            Pressed::Edited | Pressed::Shifted => {
                if !modifier {
                    self.edited();
                }
                Step::Redraw
            }
            Pressed::Submitted => Step::Finished(self.finish()),
        })
    }

    /// Whether a finger that pressed in the text is still down, so that its
    /// moves matter. A caller can skip laying the field out for any other
    /// move.
    #[must_use]
    pub const fn is_pressed(&self) -> bool {
        self.gestures.is_pressed()
    }

    /// Applies one touch, in the screen's logical coordinates, at `millis`
    /// on the caller's clock, against `layout`: the field's screen as it was
    /// drawn and laid out.
    ///
    /// A touch in the text places the caret or selects; one that lifts on
    /// nothing at all drops a selection. Anything else, and every touch on a
    /// secret, is [`Touched::Pass`], to be resolved against the keys as
    /// before.
    pub fn touch(&mut self, touch: Touch, millis: u64, layout: &Layout) -> Touched {
        if self.kind() == FieldKind::Secret {
            return Touched::Pass;
        }
        let view = self.view.get_mut();
        let (Some(fit), Some(bounds)) = (view.fit.as_ref(), entry_bounds(layout)) else {
            return Touched::Pass;
        };
        let lines = &view.lines;
        let rows = lines.visible().len().max(1);
        let line_height = (bounds.height / i32::try_from(rows).unwrap_or(1)).max(1);
        let measure = measurer(fit.metrics);
        let caret = fit.caret;
        let mut hit = |edit: &kobo_ui::edit::TextEdit, x: i32, y: i32, clamp: bool| {
            if !clamp && !bounds.contains(x, y) {
                return None;
            }
            let row = usize::try_from((y - bounds.y).max(0) / line_height)
                .unwrap_or(0)
                .min(rows - 1);
            Some(lines.boundary_at(edit, lines.top() + row, x - bounds.x, caret, &measure))
        };
        let step = self
            .gestures
            .touch(touch, millis, self.keyboard.edit_mut(), &mut hit);
        match step {
            EditStep::Changed => {
                self.edited();
                Touched::Redraw
            }
            EditStep::Quiet => Touched::Taken,
            EditStep::Pass => match touch {
                // A tap on nothing at all is a tap elsewhere, and lets go
                // of a selection; a tap on a key is the key's.
                Touch::Up { x, y } if layout.hit_test(x, y).is_none() => {
                    if self.keyboard.edit_mut().deselect() {
                        self.edited();
                        Touched::Redraw
                    } else {
                        Touched::Taken
                    }
                }
                _ => Touched::Pass,
            },
        }
    }

    /// Brings the lines up to date with the text, and has the window follow
    /// the caret the next time the field is drawn.
    fn edited(&mut self) {
        let view = self.view.get_mut();
        view.follow = true;
        if let Some(fit) = &view.fit {
            let measure = measurer(fit.metrics);
            view.lines
                .wrap(self.keyboard.edit_mut(), fit.width, &measure);
        }
    }

    fn finish(&mut self) -> Finished {
        let text = self.keyboard.take();
        match &self.save_as {
            // Exactly as typed. Spaces can be part of a password, and an
            // empty one is nothing to save, which is the same as backing out.
            Some(_) if text.is_empty() => Finished::Cancelled,
            Some(target) => Finished::Secret(target.install(&text)),
            None => Finished::Typed(text),
        }
    }

    /// The screen for this field, as the application whose screen is
    /// `behind` asked for it, fitted to `metrics`.
    #[must_use]
    pub fn screen(&self, behind: Option<&Screen>, metrics: &DisplayMetrics) -> Screen {
        let title = behind
            .and_then(|screen| screen.top_bar.as_ref())
            .map(|bar| bar.title.trim())
            .filter(|title| !title.is_empty());
        if self.kind() == FieldKind::Secret {
            self.secret_screen(title, metrics)
        } else {
            self.entry_screen(title, metrics)
        }
    }

    /// A secret, typed at its end: as much of the field as fits, from the
    /// most it can say to the least.
    fn secret_screen(&self, title: Option<&str>, metrics: &DisplayMetrics) -> Screen {
        let shapes = [
            Shape {
                title,
                hint: true,
                tail: None,
            },
            Shape {
                title,
                hint: false,
                tail: None,
            },
            Shape {
                title: None,
                hint: false,
                tail: None,
            },
            Shape {
                title: None,
                hint: false,
                tail: Some(TAILS[0]),
            },
            Shape {
                title: None,
                hint: false,
                tail: Some(TAILS[1]),
            },
        ];
        let chrome = Chrome::measuring(true);
        let mut drawn = Screen::new(0, Vec::new());
        for shape in &shapes {
            drawn = self.draw(shape);
            let fits = !drawn
                .diagnostics(metrics, &chrome)
                .issues
                .iter()
                .any(|issue| issue.severity == DiagnosticSeverity::Error);
            if fits {
                break;
            }
        }
        drawn
    }

    /// Every other field: the prompt and its guidance if they fit, and as
    /// many of the entry's lines as fit above the keys, with the caret.
    fn entry_screen(&self, title: Option<&str>, metrics: &DisplayMetrics) -> Screen {
        let mut view = self.view.borrow_mut();
        let view = &mut *view;
        let stale = view
            .fit
            .as_ref()
            .is_none_or(|fit| fit.metrics != *metrics || fit.title.as_deref() != title);
        if stale {
            view.fit = Some(self.fit(title, metrics));
        }
        let edit = self.keyboard.edit();
        let measure = measurer(*metrics);
        let width = view.fit.as_ref().map_or(1, |fit| fit.width);
        if view.lines.width() != width {
            view.lines.wrap_all(edit, width, &measure);
        }
        let lines = view.lines.lines();
        let fit = view.fit.as_mut().expect("fitted just now");
        let mut room = |shape: usize, paged: bool| {
            *fit.room[shape][usize::from(paged)]
                .get_or_insert_with(|| self.room(&shapes(title)[shape], paged, metrics))
        };
        let (shape, rows, paged) = if let Some(shape) =
            (0..3).find(|shape| room(*shape, false) >= lines)
        {
            (shape, lines, false)
        } else if let Some(shape) = (0..3).find(|shape| room(*shape, true) >= COMFORTABLE_ROWS) {
            (shape, room(shape, true), true)
        } else if room(2, true) >= MIN_PAGED_ROWS {
            (2, room(2, true), true)
        } else {
            (2, room(2, false).max(1), false)
        };
        view.lines.show(edit, rows, view.follow);
        view.follow = false;
        let (text, spans) = self.entry_text(&view.lines);
        self.draw_entry(&shapes(title)[shape], text, spans, paged)
    }

    /// How the entry fits on this panel under this title: the width of its
    /// lines, found by laying the field out so it is whatever the panel
    /// draws, and kept until the panel or the title changes.
    fn fit(&self, title: Option<&str>, metrics: &DisplayMetrics) -> Fit {
        let width = self
            .draw_entry(&shapes(title)[2], "x".to_owned(), Vec::new(), false)
            .layout_with(metrics, &Chrome::measuring(true))
            .nodes
            .iter()
            .find(|node| is_entry_bounds(node))
            .map_or(metrics.width, |node| node.rect.width);
        let measure = measurer(*metrics);
        Fit {
            metrics: *metrics,
            title: title.map(str::to_owned),
            // The caret is drawn in the line, and a space can hang at the
            // end of one, so a line leaves room for both.
            width: width - measure(CARET) - measure(" "),
            caret: measure(CARET),
            room: [[None; 2]; 3],
        }
    }

    /// The most lines of entry that fit when the field is laid out as
    /// `shape`, with or without the page controls under them. Found by
    /// halving, since room only shrinks as lines are added.
    fn room(&self, shape: &Shape<'_>, paged: bool, metrics: &DisplayMetrics) -> usize {
        let fits = |rows: usize| {
            let probe = vec!["x"; rows].join("\n");
            !self
                .draw_entry(shape, probe, Vec::new(), paged)
                .diagnostics(metrics, &Chrome::measuring(true))
                .issues
                .iter()
                .any(|issue| issue.severity == DiagnosticSeverity::Error)
        };
        let (mut low, mut high) = (0, MAX_ROWS);
        while low < high {
            let middle = (low + high).div_ceil(2);
            if fits(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low
    }

    /// The visible lines of the entry as one paragraph, a line to each
    /// line, with the caret drawn in it and the selection marked.
    fn entry_text(&self, lines: &TextView) -> (String, Vec<RichTextSpan>) {
        let edit = self.keyboard.edit();
        let text = edit.text();
        let selection = edit.selection();
        let caret = selection.is_none().then(|| edit.caret());
        let mut drawn = String::new();
        let mut spans = Vec::new();
        if text.is_empty() {
            // The caret, then the placeholder in the muted tone after it.
            drawn.push_str(CARET);
            drawn.push_str(PLACEHOLDER);
            spans.push(RichTextSpan {
                start: CARET.len(),
                end: drawn.len(),
                presentation: TextPresentation {
                    muted: true,
                    ..TextPresentation::default()
                },
            });
            return (drawn, spans);
        }
        for (row, index) in lines.visible().enumerate() {
            if row > 0 {
                drawn.push('\n');
            }
            let range = lines.line(edit, index);
            let shown_end = lines.drawn_end(edit, index);
            let here = caret.filter(|caret| lines.line_of(*caret) == index);
            let end = here.map_or(shown_end, |caret| shown_end.max(caret));
            let base = drawn.len();
            match here {
                Some(caret) => {
                    drawn.push_str(&text[range.start..caret]);
                    drawn.push_str(CARET);
                    drawn.push_str(&text[caret..end]);
                }
                None => drawn.push_str(&text[range.start..end]),
            }
            if let Some(selected) = &selection {
                let start = selected.start.max(range.start);
                let stop = selected.end.min(end);
                if start < stop {
                    spans.push(RichTextSpan {
                        start: base + start - range.start,
                        end: base + stop - range.start,
                        presentation: TextPresentation {
                            highlighted: true,
                            underline: true,
                            ..TextPresentation::default()
                        },
                    });
                }
            }
        }
        (drawn, spans)
    }

    fn draw_entry(
        &self,
        shape: &Shape<'_>,
        text: String,
        spans: Vec<RichTextSpan>,
        paged: bool,
    ) -> Screen {
        let mut entry = vec![Node::RichText {
            id: TEXT_NODE,
            spans,
            text,
            links: Vec::new(),
            presentation: ParagraphPresentation::PLAIN,
            selection: None,
            formulae: Vec::new(),
        }];
        if paged {
            entry.push(Node::Grid {
                id: NodeId(TEXT_NODE.0 + 1),
                columns: 2,
                square: false,
                cells: vec![
                    Cell::new(ActionId::from_name(PAGE_UP), "Earlier"),
                    Cell::new(ActionId::from_name(PAGE_DOWN), "Later"),
                ],
            });
        }
        self.frame(shape, entry)
    }

    fn draw(&self, shape: &Shape<'_>) -> Screen {
        let entry = self
            .field_lines(shape.tail)
            .into_iter()
            .map(|line| match line {
                Line::Plain(text) => Node::Text {
                    id: NodeId(0),
                    text,
                    links: Vec::new(),
                },
                Line::Quiet(text) => Node::Secondary {
                    id: NodeId(0),
                    text,
                },
                // One run over the whole line, so the placeholder is the same
                // paragraph at the same size as the words that replace it and
                // the rule under the field does not move on the first key.
                Line::Placeholder(text) => Node::RichText {
                    id: NodeId(0),
                    spans: vec![RichTextSpan {
                        start: 0,
                        end: text.len(),
                        presentation: TextPresentation {
                            muted: true,
                            ..TextPresentation::default()
                        },
                    }],
                    text,
                    links: Vec::new(),
                    presentation: ParagraphPresentation::PLAIN,
                    selection: None,
                    formulae: Vec::new(),
                },
            })
            .collect();
        self.frame(shape, entry)
    }

    /// The field's screen around `entry`: the prompt and the guidance the
    /// shape has room for above it, the keys below. A node in `entry` with
    /// no identity of its own is given the next one.
    fn frame(&self, shape: &Shape<'_>, entry: Vec<Node>) -> Screen {
        let mut nodes = Vec::new();
        let mut next = 2;
        let mut id = || {
            let id = NodeId(next);
            next += 1;
            id
        };
        let bar_title = match shape.title {
            // The prompt names the field, not the page: the bar already says
            // whose screen this is. Set at the first level it was a second
            // title, larger than the first, over a single line of input.
            Some(title) => {
                nodes.push(Node::Heading {
                    id: id(),
                    text: self.prompt.clone(),
                    level: 2,
                });
                title.to_owned()
            }
            None => self.prompt.clone(),
        };
        if shape.hint && !self.hint.trim().is_empty() {
            nodes.push(Node::Secondary {
                id: id(),
                text: self.hint.clone(),
            });
        }
        for mut node in entry {
            if let Node::Text { id: slot, .. }
            | Node::Secondary { id: slot, .. }
            | Node::RichText { id: slot, .. } = &mut node
            {
                if *slot == NodeId(0) {
                    *slot = id();
                }
            }
            nodes.push(node);
        }
        nodes.push(Node::Divider { id: id() });
        // The keys go to the foot of the panel, which is where thumbs are and
        // where every other platform puts them.
        nodes.push(Node::Flex { id: id() });
        for row in self.keyboard.keys(&self.submit) {
            nodes.push(Node::Grid {
                id: id(),
                columns: u8::try_from(row.len()).unwrap_or(kobo_ui::MAX_COLUMNS),
                square: false,
                cells: row.iter().map(keyboard::Key::cell).collect(),
            });
        }
        let mut bar = TopBar::new(NodeId(1), bar_title);
        if self.kind() == FieldKind::Secret {
            bar = bar.action(
                ActionId::from_name(REVEAL),
                if self.revealed { "Hide" } else { "Show" },
            );
        }
        let mut screen = Screen::new(ActionId::from_name(SCREEN_NAME).0, nodes);
        screen.top_bar = Some(bar);
        screen.owns_back = true;
        screen
    }

    /// What the field says, under the prompt.
    ///
    /// A secret is not on the panel unless the owner asks for it, and its
    /// length always is. Typing blind with no count meant a 24-character
    /// token copied by eye from a phone failed on one mis-tap with nothing on
    /// screen to say which; the count catches a dropped or doubled key
    /// against the length the service printed, and Show lets the owner read
    /// it back once, somewhere nobody is looking over their shoulder.
    fn field_lines(&self, tail: Option<usize>) -> Vec<Line> {
        let text = self.keyboard.text();
        if text.is_empty() {
            return vec![Line::Placeholder(PLACEHOLDER.to_owned())];
        }
        let shown = match tail {
            Some(keep) if self.keyboard.count() > keep => {
                let skip = self.keyboard.count() - keep;
                format!("\u{2026}{}", text.chars().skip(skip).collect::<String>())
            }
            _ => text.to_owned(),
        };
        if self.kind() != FieldKind::Secret {
            return vec![Line::Plain(shown)];
        }
        let count = match self.keyboard.count() {
            1 => "1 character entered".to_owned(),
            count => format!("{count} characters entered"),
        };
        if self.revealed {
            vec![Line::Plain(shown), Line::Quiet(count)]
        } else {
            vec![Line::Plain(count)]
        }
    }
}

/// The ways an entry is laid out, from the most it can say to the least.
const fn shapes(title: Option<&str>) -> [Shape<'_>; 3] {
    [
        Shape {
            title,
            hint: true,
            tail: None,
        },
        Shape {
            title,
            hint: false,
            tail: None,
        },
        Shape {
            title: None,
            hint: false,
            tail: None,
        },
    ]
}

/// Measures a line of an entry as the panel will set it.
fn measurer(metrics: DisplayMetrics) -> impl Fn(&str) -> i32 {
    move |text| {
        kobo_ui::with_text_scale(metrics.text_scale, || {
            kobo_ui::measure_text_in(text, FontSize::Body, Face::Text).0
        })
    }
}

/// Whether `node` is the one a laid-out entry reports its bounds in.
fn is_entry_bounds(node: &kobo_ui::LayoutNode) -> bool {
    node.id == TEXT_NODE
        && node.text_lines.is_empty()
        && matches!(node.kind, LayoutKind::RichText(_))
}

/// Where the entry's lines are on a laid-out field.
fn entry_bounds(layout: &Layout) -> Option<Rect> {
    layout
        .nodes
        .iter()
        .find(|node| is_entry_bounds(node))
        .map(|node| node.rect)
}

/// One way of laying the field out, from the most it can say to the least.
struct Shape<'a> {
    /// The application's title for the top bar, with the prompt as a heading
    /// under it. `None` puts the prompt in the bar instead.
    title: Option<&'a str>,
    hint: bool,
    /// How many characters of the entry to show, from its end.
    tail: Option<usize>,
}

enum Line {
    Plain(String),
    Quiet(String),
    /// What an empty field says in place of an entry. Muted, because set in
    /// ink at the size of an entry it reads as something already typed.
    Placeholder(String),
}

/// What an empty field shows.
const PLACEHOLDER: &str = "Type here";

/// The answer an application gets for a finished field.
///
/// `store` writes a secret and answers as [`DeviceRequest::SetSecret`] would.
/// It is passed in rather than called here because the device and the
/// simulator keep secrets in different places, and the simulator can be told
/// to fail a write the way a full card would.
pub fn answer(
    finished: Finished,
    store: impl FnOnce(&DeviceRequest) -> DeviceResult,
) -> DeviceResult {
    DeviceResult::Text(match finished {
        Finished::Typed(text) => TextOutcome::Typed(text),
        Finished::Cancelled => TextOutcome::Cancelled,
        Finished::Secret(install) => match store(&install) {
            DeviceResult::Done => TextOutcome::Saved,
            DeviceResult::Denied(reason) => TextOutcome::Denied(reason),
            DeviceResult::Failed(error) => TextOutcome::Failed(error),
            _ => TextOutcome::Failed(DeviceError::Backend),
        },
    })
}

/// A second request arriving while a field is open replaces it.
///
/// The application has moved on, and the newest field is the one it is
/// drawing. The first is answered as cancelled, so every request still gets
/// exactly one answer and in the order they were asked.
#[must_use]
pub const fn superseded() -> DeviceResult {
    DeviceResult::Text(TextOutcome::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::{
        answer, Field, Finished, Step, Touch, Touched, PAGE_DOWN, PAGE_UP, REVEAL, TEXT_NODE,
    };
    use kobo_protocol::{
        DenyReason, DeviceError, DeviceRequest, DeviceResult, SecretTarget, TextOutcome,
    };
    use kobo_ui::{
        ActionId, Chrome, DiagnosticSeverity, DisplayMetrics, FieldKind, LayoutKind, Node, Screen,
        TextScale, TopBar,
    };

    fn request(kind: FieldKind) -> DeviceRequest {
        DeviceRequest::TypeText {
            prompt: "Account key".into(),
            hint: "24 characters, from lichess.org/account/oauth/token".into(),
            initial: String::new(),
            submit: "Save".into(),
            kind,
            save_as: (kind == FieldKind::Secret).then(|| SecretTarget::named("lichess")),
        }
    }

    fn open(kind: FieldKind) -> Field {
        Field::open("lichess", &request(kind))
            .expect("a text request")
            .expect("lichess may keep a lichess token")
    }

    fn tap(field: &mut Field, name: &str) -> Option<Step> {
        field.press(ActionId::from_name(name))
    }

    fn app_screen() -> Screen {
        let mut screen = Screen::new(1, Vec::new());
        screen.top_bar = Some(TopBar::new(kobo_ui::NodeId(1), "Lichess"));
        screen
    }

    fn panels() -> Vec<(String, DisplayMetrics)> {
        kobo_profile::SUPPORTED_PROFILES
            .iter()
            .flat_map(|profile| {
                let portrait = DisplayMetrics {
                    width: i32::try_from(profile.width).expect("profile width fits layout"),
                    height: i32::try_from(profile.height).expect("profile height fits layout"),
                    pixels_per_inch: i32::from(profile.pixels_per_inch),
                    text_scale: TextScale::Default,
                };
                let landscape = DisplayMetrics {
                    width: portrait.height,
                    height: portrait.width,
                    ..portrait
                };
                [
                    (format!("{} portrait", profile.id), portrait),
                    (format!("{} landscape", profile.id), landscape),
                ]
            })
            .collect()
    }

    fn drawn(screen: &Screen) -> String {
        format!("{screen:?}")
    }

    #[test]
    fn each_kind_opens_on_the_keys_its_answer_starts_with() {
        let first_key = |kind| {
            let screen = open(kind).screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS);
            screen
                .nodes
                .iter()
                .find_map(|node| match node {
                    Node::Grid { cells, .. } => cells
                        .iter()
                        .find(|cell| cell.action == ActionId::from_name("kb.r0c0"))
                        .map(|cell| cell.label.clone()),
                    _ => None,
                })
                .expect("the first key is drawn")
        };
        assert_eq!(first_key(FieldKind::Text), "q");
        assert_eq!(first_key(FieldKind::Secret), "q");
        assert_eq!(first_key(FieldKind::Number), "1");
        assert_eq!(first_key(FieldKind::Address), "q");
        let address = drawn(&open(FieldKind::Address).screen(None, &kobo_ui::CLARA_BW_METRICS));
        assert!(address.contains("label: \"@\""), "{address}");
        assert!(!address.contains("label: \"space\""), "{address}");
    }

    #[test]
    fn typed_text_comes_back_when_submitted_and_nothing_does_when_backed_out_of() {
        let mut field = open(FieldKind::Text);
        for key in ["kb.shift", "kb.r0c0", "kb.r0c1", "kb.space", "kb.r0c2"] {
            assert_eq!(tap(&mut field, key), Some(Step::Redraw));
        }
        assert_eq!(
            tap(&mut field, "kb.enter"),
            Some(Step::Finished(Finished::Typed("Qw e".into())))
        );
        let mut field = open(FieldKind::Text);
        tap(&mut field, "kb.r0c0");
        assert_eq!(
            field.press(ActionId::BACK),
            Some(Step::Finished(Finished::Cancelled))
        );
        assert_eq!(tap(&mut field, "something.of.the.apps"), None);
        assert_eq!(
            answer(Finished::Cancelled, |_| unreachable!("nothing is stored")),
            DeviceResult::Text(TextOutcome::Cancelled)
        );
    }

    #[test]
    fn a_secret_is_stored_and_the_application_is_told_only_that_it_was() {
        let mut field = open(FieldKind::Secret);
        for key in ["kb.r0c0", "kb.r0c1", "kb.layer", "kb.r1c9"] {
            tap(&mut field, key);
        }
        let screen = drawn(&field.screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS));
        assert!(!screen.contains("qw_"), "{screen}");
        assert!(screen.contains("3 characters entered"), "{screen}");
        assert!(!format!("{field:?}").contains("qw_"));

        tap(&mut field, REVEAL);
        let shown = drawn(&field.screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS));
        assert!(shown.contains("qw_"), "{shown}");
        assert!(shown.contains("Hide"), "{shown}");

        let Some(Step::Finished(finished)) = tap(&mut field, "kb.enter") else {
            panic!("enter finishes the field")
        };
        assert!(!format!("{finished:?}").contains("qw_"));
        let mut stored = None;
        let answered = answer(finished, |install| {
            stored = Some(install.clone());
            DeviceResult::Done
        });
        assert_eq!(answered, DeviceResult::Text(TextOutcome::Saved));
        assert!(matches!(
            stored,
            Some(DeviceRequest::SetSecret { name, value }) if name == "lichess" && value.as_str() == "qw_"
        ));
        assert!(!format!("{answered:?}").contains("qw_"));
    }

    #[test]
    fn a_secret_that_cannot_be_written_says_so_without_the_value() {
        let mut field = open(FieldKind::Secret);
        tap(&mut field, "kb.r0c0");
        let Some(Step::Finished(finished)) = tap(&mut field, "kb.enter") else {
            panic!("enter finishes the field")
        };
        assert_eq!(
            answer(finished, |_| DeviceResult::Failed(DeviceError::Backend)),
            DeviceResult::Text(TextOutcome::Failed(DeviceError::Backend))
        );
        let mut empty = open(FieldKind::Secret);
        assert_eq!(
            tap(&mut empty, "kb.enter"),
            Some(Step::Finished(Finished::Cancelled)),
            "an empty secret is nothing to save"
        );
    }

    #[test]
    fn a_secret_the_application_may_not_keep_is_refused_before_anything_is_typed() {
        let refused = Field::open("todo", &request(FieldKind::Secret))
            .expect("a text request")
            .expect_err("todo may not keep a lichess token");
        assert_eq!(
            refused,
            DeviceResult::Text(TextOutcome::Denied(DenyReason::NotDeclared))
        );
        assert!(Field::open("todo", &DeviceRequest::ReadBattery).is_none());
    }

    #[test]
    fn the_field_keeps_the_applications_title_and_says_what_is_asked() {
        let screen = open(FieldKind::Text).screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS);
        assert_eq!(
            screen.top_bar.as_ref().map(|bar| bar.title.as_str()),
            Some("Lichess")
        );
        assert!(screen.owns_back, "Back is the way out of a field");
        let text = drawn(&screen);
        assert!(text.contains("Account key"), "{text}");
        assert!(text.contains("lichess.org/account/oauth/token"), "{text}");
        let untitled = open(FieldKind::Text).screen(None, &kobo_ui::CLARA_BW_METRICS);
        assert_eq!(
            untitled.top_bar.as_ref().map(|bar| bar.title.as_str()),
            Some("Account key")
        );
    }

    /// Every key on every page, at every text size, on every reader held
    /// either way: drawn on the panel, a fingertip wide, and reaching the
    /// key it is drawn as.
    #[test]
    fn every_field_fits_and_every_key_is_reachable_at_every_size_held_either_way() {
        // Measured in the runtime's own type, as the panel will draw it.
        kobo_text::install(kobo_ui::CLARA_BW_METRICS).expect("the runtime's fonts");
        let pages: [&[&str]; 4] = [&[], &["kb.shift"], &["kb.layer"], &["kb.layer", "kb.shift"]];
        for (name, mut metrics) in panels() {
            for scale in TextScale::STEPS {
                metrics.text_scale = scale;
                for kind in [
                    FieldKind::Text,
                    FieldKind::Secret,
                    FieldKind::Number,
                    FieldKind::Address,
                ] {
                    for page in pages {
                        let mut field = open(kind);
                        for _ in 0..30 {
                            tap(&mut field, "kb.r0c1");
                        }
                        tap(&mut field, REVEAL);
                        for key in page {
                            tap(&mut field, key);
                        }
                        let screen = field.screen(Some(&app_screen()), &metrics);
                        let chrome = Chrome::for_screen(&screen, false, None);
                        let errors: Vec<_> = screen
                            .diagnostics(&metrics, &Chrome::measuring(true))
                            .issues
                            .into_iter()
                            .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                            .collect();
                        assert!(
                            errors.is_empty(),
                            "{name} at {scale:?}, {kind:?} {page:?}: {errors:?}"
                        );
                        let layout = screen.layout_with(&metrics, &chrome);
                        for node in &layout.nodes {
                            let LayoutKind::Cell(action, ..) = node.kind else {
                                continue;
                            };
                            let rect = node.rect;
                            assert!(
                                rect.width >= metrics.touch_target_minimum()
                                    && rect.height >= metrics.touch_target_minimum(),
                                "{name} at {scale:?}: a key is smaller than a fingertip"
                            );
                            assert_eq!(
                                layout.hit_test(rect.x + rect.width / 2, rect.y + rect.height / 2),
                                Some(action),
                                "{name} at {scale:?}: a key is covered"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_field_says_type_here_in_the_muted_tone_and_typing_replaces_it_in_ink() {
        let muted = |screen: &Screen| {
            screen.nodes.iter().any(|node| {
                matches!(node, Node::RichText { text, spans, .. }
                    if text == "|Type here"
                        && spans.iter().all(|span| span.presentation.muted)
                        && spans.iter().map(|span| &text[span.start..span.end]).eq(["Type here"]))
            })
        };
        let mut field = open(FieldKind::Text);
        let empty = field.screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS);
        assert!(
            muted(&empty),
            "the caret in ink, then the placeholder: {empty:?}"
        );
        tap(&mut field, "kb.r0c0");
        let typed = field.screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS);
        assert!(!muted(&typed), "{typed:?}");
        assert_eq!(entry(&typed), "q|");
    }

    /// The entry as drawn, caret and all.
    fn entry(screen: &Screen) -> String {
        screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::RichText { id, text, .. } if *id == TEXT_NODE => Some(text.clone()),
                _ => None,
            })
            .expect("the entry is drawn")
    }

    fn opened_with(kind: FieldKind, initial: &str) -> Field {
        let DeviceRequest::TypeText {
            prompt,
            hint,
            submit,
            ..
        } = request(FieldKind::Text)
        else {
            unreachable!()
        };
        Field::open(
            "lichess",
            &DeviceRequest::TypeText {
                prompt,
                hint,
                initial: initial.to_owned(),
                submit,
                kind,
                save_as: None,
            },
        )
        .expect("a text request")
        .expect("a plain field is never refused")
    }

    /// The field drawn on a Clara and laid out, as a touch is resolved
    /// against it.
    fn drawn_layout(field: &Field) -> (Screen, kobo_ui::Layout) {
        let screen = field.screen(Some(&app_screen()), &kobo_ui::CLARA_BW_METRICS);
        let layout = screen.layout_with(
            &kobo_ui::CLARA_BW_METRICS,
            &Chrome::for_screen(&screen, false, None),
        );
        (screen, layout)
    }

    /// Where `offset` of the entry's text is drawn, on line `row` of the
    /// window, as the caret and selection measure it.
    fn point(field: &Field, row: usize, offset: usize) -> (i32, i32) {
        let (_, layout) = drawn_layout(field);
        let bounds = super::entry_bounds(&layout).expect("the entry is laid out");
        let view = field.view.borrow();
        let lines = &view.lines;
        let index = lines.top() + row;
        let start = lines.line(field.keyboard.edit(), index).start;
        let measure = super::measurer(kobo_ui::CLARA_BW_METRICS);
        let rows = i32::try_from(lines.visible().len()).expect("a few lines");
        let height = bounds.height / rows;
        let edit = field.keyboard.edit();
        let caret = edit.selection().is_none()
            && lines.line_of(edit.caret()) == index
            && edit.caret() < offset;
        (
            bounds.x
                + measure(&field.keyboard.text()[start..offset])
                + if caret { measure(super::CARET) } else { 0 },
            bounds.y + height * i32::try_from(row).expect("a few lines") + height / 2,
        )
    }

    fn touch(field: &mut Field, touch: Touch, millis: u64) -> Touched {
        let (_, layout) = drawn_layout(field);
        field.touch(touch, millis, &layout)
    }

    fn tap_at(field: &mut Field, (x, y): (i32, i32), millis: u64) {
        assert_eq!(
            touch(field, Touch::Down { x, y }, millis),
            Touched::Redraw,
            "a tap at ({x}, {y}) moved nothing"
        );
        touch(field, Touch::Up { x, y }, millis + 40);
    }

    #[test]
    fn a_tap_in_the_text_moves_the_caret_and_typing_goes_in_there() {
        let mut field = opened_with(FieldKind::Text, "milk eggs");
        assert_eq!(entry(&drawn_layout(&field).0), "milk eggs|");
        let at = point(&field, 0, 4);
        tap_at(&mut field, at, 0);
        assert_eq!(entry(&drawn_layout(&field).0), "milk| eggs");
        tap(&mut field, "kb.r0c5");
        assert_eq!(entry(&drawn_layout(&field).0), "milky| eggs");
        // A little right of the gap between two letters is still that gap.
        let (x, y) = point(&field, 0, 1);
        tap_at(&mut field, (x + 2, y), 5000);
        tap(&mut field, "kb.backspace");
        assert_eq!(
            tap(&mut field, "kb.enter"),
            Some(Step::Finished(Finished::Typed("ilky eggs".into())))
        );
    }

    #[test]
    fn a_drag_selects_typing_replaces_it_and_a_double_tap_takes_a_word() {
        let mut field = opened_with(FieldKind::Prose, "buy milk and eggs");
        // Each point is taken from the field as it is drawn at that moment:
        // the caret is drawn in the line until the selection replaces it.
        let (x, y) = point(&field, 0, 4);
        assert_eq!(touch(&mut field, Touch::Down { x, y }, 0), Touched::Redraw);
        let (x, y) = point(&field, 0, 8);
        assert_eq!(touch(&mut field, Touch::Move { x, y }, 30), Touched::Redraw);
        let (x, y) = point(&field, 0, 8);
        assert_eq!(touch(&mut field, Touch::Up { x, y }, 60), Touched::Taken);
        assert_eq!(field.keyboard.edit().selection(), Some(4..8));
        let screen = drawn_layout(&field).0;
        assert_eq!(
            entry(&screen),
            "buy milk and eggs",
            "no caret over a selection"
        );
        let marked = screen.nodes.iter().any(|node| {
            matches!(node, Node::RichText { text, spans, .. }
                if spans.iter().any(|span| &text[span.start..span.end] == "milk"
                    && span.presentation.highlighted))
        });
        assert!(marked, "{screen:?}");
        tap(&mut field, "kb.r1c5");
        assert_eq!(entry(&drawn_layout(&field).0), "buy h| and eggs");

        let word = point(&field, 0, 14);
        tap_at(&mut field, word, 1000);
        touch(
            &mut field,
            Touch::Down {
                x: word.0,
                y: word.1,
            },
            1100,
        );
        touch(
            &mut field,
            Touch::Up {
                x: word.0,
                y: word.1,
            },
            1140,
        );
        assert_eq!(field.keyboard.edit().selection(), Some(10..14));
        tap(&mut field, "kb.backspace");
        assert_eq!(field.keyboard.text(), "buy h and ");

        // A tap on nothing at all lets go of a selection.
        field.keyboard.edit_mut().select(0, 3);
        let (_, layout) = drawn_layout(&field);
        let divider = layout
            .nodes
            .iter()
            .find(|node| node.kind == LayoutKind::Divider)
            .expect("a rule under the entry")
            .rect;
        let y = divider.y + divider.height / 2;
        touch(&mut field, Touch::Down { x: 5, y }, 3000);
        assert_eq!(
            touch(&mut field, Touch::Up { x: 5, y }, 3040),
            Touched::Redraw
        );
        assert_eq!(field.keyboard.edit().selection(), None);
    }

    #[test]
    fn a_secret_takes_no_touch_and_draws_no_caret() {
        let mut field = open(FieldKind::Secret);
        tap(&mut field, "kb.r0c0");
        tap(&mut field, REVEAL);
        let (screen, layout) = drawn_layout(&field);
        assert!(!format!("{screen:?}").contains('|'), "{screen:?}");
        for event in [
            Touch::Down { x: 60, y: 300 },
            Touch::Move { x: 400, y: 300 },
            Touch::Up { x: 400, y: 300 },
        ] {
            assert_eq!(field.touch(event, 0, &layout), Touched::Pass);
        }
    }

    #[test]
    fn a_long_paragraph_is_a_window_that_follows_the_caret_and_pages() {
        let words: Vec<String> = (0..400).map(|word| format!("w{word}")).collect();
        let paragraph = words.join(" ");
        let mut field = opened_with(FieldKind::Prose, &paragraph);
        let (screen, _) = drawn_layout(&field);
        let shown = entry(&screen);
        assert!(
            shown.ends_with("w399|"),
            "the window opens on the caret: {shown}"
        );
        assert!(!shown.contains("w0 "), "{shown}");
        assert!(
            format!("{screen:?}").contains("label: \"Earlier\""),
            "{screen:?}"
        );
        for _ in 0..200 {
            if tap(&mut field, PAGE_UP).is_none() {
                break;
            }
        }
        let top = entry(&drawn_layout(&field).0);
        assert!(top.starts_with("w0 w1 "), "{top}");
        assert!(!top.contains('|'), "the caret is further down: {top}");
        assert_eq!(tap(&mut field, PAGE_UP), None, "already at the top");
        // Put the caret after "w1" and type: the paragraph is edited there,
        // and the window stays where the caret is.
        let at = point(&field, 0, 5);
        tap_at(&mut field, at, 0);
        tap(&mut field, "kb.r0c2");
        let edited = entry(&drawn_layout(&field).0);
        assert!(edited.starts_with("w0 w1e| w2"), "{edited}");
        assert!(field.keyboard.text().ends_with("w398 w399"));
        // The caret keys step through it and take the window with them.
        tap(&mut field, PAGE_DOWN);
        tap(&mut field, "kb.right");
        assert!(entry(&drawn_layout(&field).0).starts_with("w0 w1e |w2"));
        let Some(Step::Finished(Finished::Typed(text))) = tap(&mut field, "kb.enter") else {
            panic!("enter finishes the field")
        };
        assert_eq!(text, paragraph.replacen("w1", "w1e", 1));
    }

    /// A long entry on every reader, at every size, either way up: it fits,
    /// every line it draws is one line on the panel, so a finger finds the
    /// letter it was aimed at, and the page controls are a fingertip wide.
    #[test]
    fn a_long_entry_fits_and_draws_each_line_as_one_line_everywhere() {
        kobo_text::install(kobo_ui::CLARA_BW_METRICS).expect("the runtime's fonts");
        let paragraph = "Retyping a paragraph in the middle, where a word went wrong, \
                         without starting again from its first letter. "
            .repeat(30);
        for (name, mut metrics) in panels() {
            for scale in TextScale::STEPS {
                metrics.text_scale = scale;
                let field = opened_with(FieldKind::Prose, &paragraph);
                let screen = field.screen(Some(&app_screen()), &metrics);
                let errors: Vec<_> = screen
                    .diagnostics(&metrics, &Chrome::measuring(true))
                    .issues
                    .into_iter()
                    .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                    .collect();
                assert!(errors.is_empty(), "{name} at {scale:?}: {errors:?}");
                let layout =
                    screen.layout_with(&metrics, &Chrome::for_screen(&screen, false, None));
                let drawn = entry(&screen);
                let mut tops: Vec<i32> = layout
                    .nodes
                    .iter()
                    .filter(|node| node.id == TEXT_NODE && !node.text_lines.is_empty())
                    .map(|node| node.rect.y)
                    .collect();
                tops.dedup();
                assert_eq!(
                    tops.len(),
                    drawn.lines().count(),
                    "{name} at {scale:?}: a line was broken again on the panel"
                );
                for node in &layout.nodes {
                    if let LayoutKind::Cell(..) = node.kind {
                        assert!(
                            node.rect.width >= metrics.touch_target_minimum()
                                && node.rect.height >= metrics.touch_target_minimum(),
                            "{name} at {scale:?}: a control is smaller than a fingertip"
                        );
                    }
                }
            }
        }
    }
}
