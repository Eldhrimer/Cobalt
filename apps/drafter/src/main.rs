//! Drafter: read one section of the book at a time, mark it up, send the marks
//! to a gateway that revises under the project's rules, and keep or discard
//! each revision. Nothing here talks to GitHub or a model; the gateway does.
//!
//! The interaction model, in one paragraph. Hold a word and a menu offers what
//! to do with its paragraph: highlight the sentence, the paragraph or a range
//! (hold the first word, then the last), split before the word, or open the
//! block options (move, merge, retype, note, delete). The outline lists every
//! block with a menu of its own, which is where tables and rules are reached.
//! Moving a block is two taps: pick it up, tap the block it should follow.
//! Dragging is not offered because a panel that repaints in a third of a
//! second cannot follow a finger.

mod model;
mod pages;
mod views;
mod wire;

use kobo_json::{ObjectBuilder, Value};
use kobo_sdk::keyboard::{Keyboard, Pressed, TextEntry, Typing};
use kobo_sdk::{
    action_id, ActionId, Context, Failure, Heartbeat, KoboApp, Screen, ScreenBuilder, StoreResult,
    TaskError, TaskId, TaskOutcome,
};
use kobo_ui::TextHit;
use model::{Ask, Block, Op, Working};
use pages::Item;
use std::process::ExitCode;
use views::{
    CHECK, CLEAR, COMMIT, DISCARD, GATEWAY, LIST, MORE, NEXT, NO, OUTLINE, PREV, QUEUE, REFRESH,
    RELOAD, REVIEW, SEND, STOP, UNDO, YES,
};
use wire::{CommitReply, JobState, ReviewReply, SectionData, SectionList, Take};

const GATEWAY_KEY: &str = "gateway";
const LIST_KEY: &str = "list";
const HINT_KEY: &str = "hint";
const INFLIGHT_KEY: &str = "inflight";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum View {
    #[default]
    Opening,
    Setup,
    Sections,
    Reading,
    Outline,
    Placing,
    Asking,
    Typing,
    Queue,
    Waiting,
    Proposal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Menu {
    Word {
        block: usize,
        part: usize,
        start: usize,
        end: usize,
    },
    Block {
        block: usize,
    },
    Send,
    Clear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub block: String,
    pub part: usize,
    pub start: usize,
    pub end: usize,
    pub word: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Range {
    pub block: String,
    pub part: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Typed {
    Note { block: String },
    Retype { block: String },
    Custom,
    Want { after: Option<String> },
    Insert { after: Option<String> },
}

impl Typed {
    #[must_use]
    pub const fn prompt(&self) -> (&'static str, &'static str) {
        match self {
            Self::Note { .. } => ("Note", "What should the reviser know about this block?"),
            Self::Retype { .. } => ("Retype", "Rewrite it yourself"),
            Self::Custom => ("Note", "What do you want done with this passage?"),
            Self::Want { .. } => ("New paragraph", "What should the new paragraph say?"),
            Self::Insert { .. } => ("New paragraph", "Write the paragraph"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inflight {
    pub review_id: String,
    pub job_id: String,
    pub section_id: String,
}

impl Inflight {
    fn encode(&self) -> Vec<u8> {
        ObjectBuilder::new()
            .set("review_id", self.review_id.as_str())
            .set("job_id", self.job_id.as_str())
            .set("section_id", self.section_id.as_str())
            .build()
            .to_json()
            .into_bytes()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let root = kobo_json::parse(std::str::from_utf8(bytes).ok()?).ok()?;
        let get = |key: &str| root.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            review_id: get("review_id")?,
            job_id: get("job_id")?,
            section_id: get("section_id")?,
        })
    }
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // independent screen flags, not a state machine
pub struct Drafter {
    pub(crate) view: View,
    pub(crate) gateway: String,
    pub(crate) keyboard: Keyboard,
    pub(crate) entry: TextEntry,
    pub(crate) typing: Option<Typed>,
    pub(crate) typing_return: View,

    pub(crate) list: SectionList,
    pub(crate) list_page: usize,
    pub(crate) current: Option<SectionData>,
    pub(crate) ops: Vec<Op>,
    pub(crate) work: Working,
    pub(crate) pages: Vec<Vec<Item>>,
    pub(crate) pages_valid: bool,
    pub(crate) page: usize,

    pub(crate) menu: Option<Menu>,
    pub(crate) more_open: bool,
    pub(crate) anchor: Option<Anchor>,
    pub(crate) range: Option<Range>,
    pub(crate) placing: Option<String>,
    pub(crate) outline_page_index: usize,
    pub(crate) row_menu: Option<usize>,
    pub(crate) queue_page_index: usize,
    pub(crate) ask_page: usize,
    pub(crate) queue_menu: Option<usize>,
    pub(crate) proposal_page_index: usize,

    pub(crate) inflight: Option<Inflight>,
    pub(crate) proposal: Option<wire::Proposal>,
    pub(crate) takes: Vec<(String, Take)>,
    pub(crate) waiting: String,
    pub(crate) clock: Heartbeat,

    pub(crate) fetching_list: Option<TaskId>,
    fetching_section: Option<TaskId>,
    posting: Option<TaskId>,
    polling: Option<TaskId>,
    committing: Option<TaskId>,

    opening: Option<String>,
    /// `None`: not read yet. `Some(None)`: read, and nothing was saved.
    #[allow(clippy::option_option)]
    cached_section: Option<Option<SectionData>>,
    cached_ops: Option<Vec<Op>>,
    confirmed_locked: bool,

    pub(crate) notice: Option<String>,
    pub(crate) hint_seen: bool,
}

fn is(action: ActionId, name: &str) -> bool {
    action == action_id(name)
}

fn index_of(action: ActionId, prefix: &str, limit: usize) -> Option<usize> {
    (0..limit).find(|n| is(action, &format!("{prefix}.{n}")))
}

impl Drafter {
    // -------------------------------------------------------------- drawing

    fn show(&mut self, c: &mut Context) {
        if self.view == View::Reading {
            self.ensure_pages(c);
        }
        let screen: Screen = match self.view {
            View::Opening => ScreenBuilder::new("drafter-opening")
                .top_bar("Drafter")
                .activity("Opening", None)
                .build(),
            View::Setup => self.setup_screen(),
            View::Sections => self.sections_screen(c).0,
            View::Reading => self.reading_screen(),
            View::Outline => self.outline_screen(c),
            View::Placing => {
                let moving = self.placing.clone().unwrap_or_default();
                self.placing_screen(c, &moving)
            }
            View::Asking => self.asking_screen(c),
            View::Typing => self.typing_screen(),
            View::Queue => self.queue_screen(c),
            View::Waiting => self.waiting_screen(),
            View::Proposal => self.proposal_screen(c),
        };
        c.set_screen(
            screen.with_own_back(self.view != View::Opening && self.view != View::Sections),
        );
    }

    fn ensure_pages(&mut self, c: &Context) {
        if self.pages_valid {
            return;
        }
        let top = self
            .pages
            .get(self.page)
            .and_then(|p| p.first())
            .map(|i| i.block());
        let top_id = top
            .and_then(|b| self.work.blocks.get(b))
            .map(|b| b.id.clone());
        self.pages = self.paginate_reading(c);
        self.pages_valid = true;
        self.page = top_id
            .and_then(|id| self.work.blocks.iter().position(|b| b.id == id))
            .and_then(|block| {
                self.pages
                    .iter()
                    .position(|p| p.iter().any(|i| i.block() == block))
            })
            .unwrap_or(0)
            .min(self.pages.len().saturating_sub(1));
    }

    fn invalidate(&mut self) {
        self.pages_valid = false;
        self.queue_menu = None;
        self.row_menu = None;
    }

    // ---------------------------------------------------------------- state

    fn base_blocks(&self) -> &[Block] {
        self.current.as_ref().map_or(&[], |s| s.blocks.as_slice())
    }

    fn rebuild(&mut self) -> Result<(), String> {
        self.work = Working::replay(self.base_blocks(), &self.ops)?;
        self.invalidate();
        Ok(())
    }

    fn section_id(&self) -> Option<String> {
        self.current.as_ref().map(|s| s.id.clone())
    }

    fn save_ops(&self, c: &mut Context) {
        if let Some(id) = self.section_id() {
            if self.ops.is_empty() {
                c.store().forget(format!("ops.{id}"));
            } else {
                c.store().save(
                    format!("ops.{id}"),
                    model::ops_to_json(&self.ops).into_bytes(),
                );
            }
        }
    }

    fn fresh_id(&self) -> String {
        let made = self
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Split { .. } | Op::Insert { .. }))
            .count();
        format!("d{}", made + 1)
    }

    /// Queues an operation and rebuilds the working copy; a refused one is
    /// taken straight back out and its reason shown.
    fn push_op(&mut self, c: &mut Context, op: Op, done: &str) -> bool {
        self.ops.push(op);
        match self.rebuild() {
            Ok(()) => {
                self.save_ops(c);
                self.notice = Some(done.to_owned());
                true
            }
            Err(reason) => {
                self.ops.pop();
                let _ = self.rebuild();
                self.notice = Some(format!("Not possible: {reason}."));
                false
            }
        }
    }

    fn undo(&mut self, c: &mut Context) {
        if self.ops.pop().is_some() {
            if self.rebuild().is_ok() {
                self.save_ops(c);
                self.notice = Some("Undid the last change.".into());
            }
        } else {
            self.notice = Some("Nothing to undo.".into());
        }
    }

    fn fail_text(error: TaskError) -> String {
        if matches!(error, TaskError::NoCredential) {
            "Finish setup on your computer with kobo drafter login.".to_owned()
        } else {
            format!("Couldn't reach the gateway: {}", Failure::of(error).advice)
        }
    }

    // ------------------------------------------------------------- fetching

    fn check_list(&mut self, c: &mut Context) {
        if self.fetching_list.is_none() && !self.gateway.is_empty() {
            self.fetching_list = c.spawn(wire::sections(&self.gateway));
        }
    }

    fn fetch_section(&mut self, c: &mut Context, id: &str) {
        if let Some(old) = self.fetching_section.take() {
            c.cancel(old);
        }
        self.fetching_section = c.spawn(wire::section(&self.gateway, id));
    }

    fn open_section(&mut self, c: &mut Context, index: usize) {
        let Some(info) = self.list.items.get(index) else {
            return;
        };
        let id = info.id.clone();
        self.opening = Some(id.clone());
        self.cached_section = None;
        self.cached_ops = None;
        self.current = None;
        self.ops.clear();
        self.work = Working::default();
        self.menu = None;
        self.anchor = None;
        self.range = None;
        self.proposal = None;
        self.takes.clear();
        self.confirmed_locked = false;
        self.notice = None;
        self.page = 0;
        self.invalidate();
        self.view = View::Opening;
        c.store().load(format!("sec.{id}"));
        c.store().load(format!("ops.{id}"));
        self.show(c);
    }

    fn finish_open(&mut self, c: &mut Context) {
        if self.cached_section.is_none() || self.cached_ops.is_none() {
            return;
        }
        let (Some(section), Some(ops)) = (self.cached_section.take(), self.cached_ops.take())
        else {
            return;
        };
        let Some(id) = self.opening.clone() else {
            return;
        };
        self.ops = ops;
        if let Some(data) = section {
            self.current = Some(data);
            if self.rebuild().is_err() {
                self.ops.clear();
                self.save_ops(c);
                let _ = self.rebuild();
                self.notice =
                    Some("Saved changes no longer fit this section and were dropped.".into());
            }
            self.view = View::Reading;
            self.show(c);
        }
        self.fetch_section(c, &id);
        if self.inflight.as_ref().is_some_and(|f| f.section_id == id) {
            self.notice = Some("A revision is waiting. Open the queue to check it.".into());
        }
    }

    fn adopt_section(&mut self, c: &mut Context, data: SectionData, raw_saved: bool) {
        let same_rev = self
            .current
            .as_ref()
            .is_some_and(|s| s.rev == data.rev && s.id == data.id);
        if !same_rev && self.current.is_some() && !self.ops.is_empty() {
            self.notice =
                Some("This section changed on GitHub. Sending will ask you to reload it.".into());
            self.show(c);
            return;
        }
        if same_rev && !raw_saved {
            return;
        }
        let id = data.id.clone();
        self.current = Some(data);
        if !self.ops.is_empty() && self.rebuild().is_err() {
            self.ops.clear();
            self.save_ops(c);
        }
        let _ = self.rebuild();
        if self.view == View::Opening && self.opening.as_deref() == Some(id.as_str()) {
            self.view = View::Reading;
        }
        self.show(c);
    }

    // ------------------------------------------------------------- selection

    fn word_hold(&mut self, c: &mut Context, hit: TextHit) {
        let (block, part, prefix) = pages::unpack(hit.context);
        let Some(b) = self.work.blocks.get(block) else {
            return;
        };
        let Some(p) = b.parts.get(part) else { return };
        let start = usize::try_from(hit.start)
            .unwrap_or(0)
            .saturating_sub(2 * prefix);
        let end = usize::try_from(hit.end)
            .unwrap_or(0)
            .saturating_sub(2 * prefix)
            .max(start);
        if end > p.text.len() || !p.text.is_char_boundary(start) || !p.text.is_char_boundary(end) {
            return;
        }
        self.menu = Some(Menu::Word {
            block,
            part,
            start,
            end,
        });
        if !self.hint_seen {
            self.hint_seen = true;
            c.store().save(HINT_KEY, b"1".to_vec());
        }
        self.show(c);
    }

    fn begin_range(&mut self, c: &mut Context, block: usize, part: usize, from: usize, to: usize) {
        let b = &self.work.blocks[block];
        let text = &b.parts[part].text;
        let slice = &text[from..to];
        let start = from + (slice.len() - slice.trim_start().len());
        let trimmed = slice.trim();
        if trimmed.is_empty() {
            self.menu = None;
            self.show(c);
            return;
        }
        self.range = Some(Range {
            block: b.id.clone(),
            part,
            start,
            end: start + trimmed.len(),
            text: trimmed.to_owned(),
        });
        self.menu = None;
        self.anchor = None;
        self.ask_page = 0;
        self.view = View::Asking;
        self.show(c);
    }

    fn word_menu_action(&mut self, c: &mut Context, a: ActionId) -> bool {
        let Some(Menu::Word {
            block,
            part,
            start,
            end,
        }) = self.menu.clone()
        else {
            return false;
        };
        let id = self.work.blocks[block].id.clone();
        let text_len = self.work.blocks[block].parts[part].text.len();
        if is(a, "w.sentence") {
            let text = &self.work.blocks[block].parts[part].text;
            let (from, to) = model::sentence_around(text, start);
            self.begin_range(c, block, part, from, to);
        } else if is(a, "w.paragraph") {
            self.begin_range(c, block, part, 0, text_len);
        } else if is(a, "w.start") {
            let word = self.work.blocks[block].parts[part].text[start..end].to_owned();
            self.anchor = Some(Anchor {
                block: id,
                part,
                start,
                end,
                word,
            });
            self.menu = None;
            self.show(c);
        } else if is(a, "w.end") {
            match self.anchor.clone() {
                Some(anchor) if anchor.block == id && anchor.part == part => {
                    self.begin_range(c, block, part, anchor.start.min(start), anchor.end.max(end));
                }
                _ => {
                    self.anchor = None;
                    self.menu = None;
                    self.notice = Some("A highlight stays inside one paragraph.".into());
                    self.show(c);
                }
            }
        } else if is(a, "w.split") {
            self.menu = None;
            self.push_op(
                c,
                Op::Split {
                    block: id,
                    at: start,
                    new: self.fresh_id(),
                },
                "Paragraph split.",
            );
            self.show(c);
        } else if is(a, "w.block") {
            self.menu = Some(Menu::Block { block });
            self.show(c);
        } else {
            return false;
        }
        true
    }

    fn start_typing(&mut self, c: &mut Context, kind: Typed, initial: Option<String>) {
        match initial {
            Some(text) => self.entry.open_with(text),
            None => self.entry.open(),
        }
        self.typing_return = if matches!(self.view, View::Outline) {
            View::Outline
        } else {
            View::Reading
        };
        if matches!(kind, Typed::Custom) {
            self.typing_return = View::Reading;
        }
        self.typing = Some(kind);
        self.menu = None;
        self.view = View::Typing;
        self.show(c);
    }

    fn block_action(&mut self, c: &mut Context, a: ActionId, block: usize) -> bool {
        let Some(b) = self.work.blocks.get(block) else {
            return false;
        };
        let id = b.id.clone();
        let text = b.parts.first().map(|p| p.text.clone());
        if is(a, "b.move") || is(a, "ov.move") {
            self.placing = Some(id);
            self.outline_page_index = 0;
            self.menu = None;
            self.row_menu = None;
            self.view = View::Placing;
            self.show(c);
        } else if is(a, "b.merge") || is(a, "ov.merge") {
            self.menu = None;
            self.push_op(
                c,
                Op::Merge { block: id },
                "Merged into the paragraph above.",
            );
            self.after_structure(c);
        } else if is(a, "b.retype") || is(a, "ov.retype") {
            self.start_typing(c, Typed::Retype { block: id }, text);
        } else if is(a, "b.note") || is(a, "ov.note") {
            self.start_typing(c, Typed::Note { block: id }, None);
        } else if is(a, "b.delete") || is(a, "ov.delete") {
            self.menu = None;
            self.push_op(c, Op::Delete { block: id }, "Deleted.");
            self.after_structure(c);
        } else if is(a, "ov.add") {
            self.start_typing(c, Typed::Insert { after: Some(id) }, None);
        } else if is(a, "ov.want") {
            self.start_typing(c, Typed::Want { after: Some(id) }, None);
        } else {
            return false;
        }
        true
    }

    fn after_structure(&mut self, c: &mut Context) {
        if self.view == View::Outline {
            let count = self.outline_pages(c).len();
            self.outline_page_index = self.outline_page_index.min(count.saturating_sub(1));
        }
        self.show(c);
    }

    fn typed(&mut self, c: &mut Context, text: String) {
        let kind = self.typing.take();
        let range = self.range.take();
        let done = match (kind, range) {
            (Some(Typed::Note { block }), _) => {
                self.push_op(c, Op::Note { block, note: text }, "Note queued.")
            }
            (Some(Typed::Retype { block }), _) => {
                self.push_op(c, Op::Edit { block, text }, "Retyped.")
            }
            (Some(Typed::Custom), Some(r)) => self.push_op(
                c,
                Op::Mark {
                    block: r.block,
                    part: r.part,
                    start: r.start,
                    end: r.end,
                    text: r.text,
                    ask: Ask::Custom,
                    note: text,
                },
                "Highlight and note queued.",
            ),
            (Some(Typed::Want { after }), _) => {
                self.push_op(c, Op::Want { after, note: text }, "Request queued.")
            }
            (Some(Typed::Insert { after }), _) => {
                let new = self.fresh_id();
                self.push_op(c, Op::Insert { after, text, new }, "Paragraph added.")
            }
            _ => false,
        };
        let _ = done;
        self.view = self.typing_return;
        self.after_structure(c);
    }

    fn mark_with(&mut self, c: &mut Context, ask: Ask) {
        let Some(r) = self.range.clone() else { return };
        if ask == Ask::Custom {
            self.start_typing(c, Typed::Custom, None);
            return;
        }
        self.range = None;
        let note = format!("Queued: {}.", ask.label().to_lowercase());
        self.push_op(
            c,
            Op::Mark {
                block: r.block,
                part: r.part,
                start: r.start,
                end: r.end,
                text: r.text,
                ask,
                note: String::new(),
            },
            &note,
        );
        self.view = View::Reading;
        self.show(c);
    }

    // --------------------------------------------------------------- review

    fn send(&mut self, c: &mut Context) {
        let (Some(section), false) = (self.current.clone(), self.ops.is_empty()) else {
            return;
        };
        if section.status.eq_ignore_ascii_case("locked") && !self.confirmed_locked {
            self.menu = Some(Menu::Send);
            self.show(c);
            return;
        }
        self.menu = None;
        let id = wire::review_id(&self.gateway, &section.id, &section.rev, &self.ops);
        self.posting = c.spawn(wire::review(
            &self.gateway,
            &id,
            &section.id,
            &section.rev,
            &self.ops,
        ));
        self.inflight = Some(Inflight {
            review_id: id,
            job_id: String::new(),
            section_id: section.id,
        });
        self.waiting = "Sending your changes".into();
        self.proposal = None;
        self.takes.clear();
        self.notice = None;
        self.view = View::Waiting;
        self.clock.start(c);
        self.show(c);
    }

    fn poll(&mut self, c: &mut Context) {
        let Some(inflight) = &self.inflight else {
            return;
        };
        if self.polling.is_none() && !inflight.job_id.is_empty() {
            self.polling = c.spawn(wire::job(&self.gateway, &inflight.job_id.clone()));
        }
    }

    fn stop_waiting(&mut self, c: &mut Context) {
        self.clock.stop(c);
        if let Some(task) = self.polling.take() {
            c.cancel(task);
        }
    }

    fn review_outcome(&mut self, c: &mut Context, out: TaskOutcome) {
        match out {
            TaskOutcome::Completed(bytes) => match wire::review_reply(&bytes) {
                Some(ReviewReply::Accepted(job) | ReviewReply::Duplicate(job)) => {
                    if let Some(f) = &mut self.inflight {
                        f.job_id = job;
                    }
                    if let Some(f) = &self.inflight {
                        c.store().save(INFLIGHT_KEY, f.encode());
                    }
                    self.waiting = "Waiting for the reviser".into();
                    self.poll(c);
                    self.show(c);
                }
                Some(ReviewReply::Stale(message)) => {
                    self.fail_review(
                        c,
                        &format!(
                            "{} Reload it from the menu, then redo your changes.",
                            if message.is_empty() {
                                "The section changed on GitHub.".to_owned()
                            } else {
                                message
                            }
                        ),
                    );
                }
                Some(ReviewReply::Rejected(message)) => {
                    self.fail_review(c, &format!("The gateway refused the changes: {message}"));
                }
                None => self.fail_review(
                    c,
                    "The gateway answered with something Drafter cannot read.",
                ),
            },
            TaskOutcome::Failed(error) => {
                let text = Self::fail_text(error);
                self.fail_review(c, &text);
            }
            TaskOutcome::Cancelled => {}
        }
    }

    fn fail_review(&mut self, c: &mut Context, message: &str) {
        self.stop_waiting(c);
        self.inflight = None;
        c.store().forget(INFLIGHT_KEY);
        self.notice = Some(message.to_owned());
        self.view = View::Queue;
        self.show(c);
    }

    fn job_outcome(&mut self, c: &mut Context, out: TaskOutcome) {
        let TaskOutcome::Completed(bytes) = out else {
            if let TaskOutcome::Failed(TaskError::NotFound) = out {
                self.fail_review(c, "The gateway no longer has this review. Send it again.");
            } else if let TaskOutcome::Failed(error) = out {
                if self.view == View::Queue {
                    self.notice = Some(Self::fail_text(error));
                    self.show(c);
                }
            }
            return;
        };
        let Some(job) = wire::job_reply(&bytes) else {
            self.notice = Some("The gateway's answer could not be read.".into());
            self.show(c);
            return;
        };
        match job.state {
            JobState::Queued | JobState::Running => {
                self.waiting = if job.message.is_empty() {
                    "Revising".into()
                } else {
                    job.message
                };
                if self.view == View::Waiting {
                    self.show(c);
                }
            }
            JobState::Failed => {
                let reason = if job.message.is_empty() {
                    "The revision failed.".to_owned()
                } else {
                    job.message
                };
                self.fail_review(c, &reason);
            }
            JobState::Done => {
                self.stop_waiting(c);
                self.proposal = job.proposal;
                self.proposal_page_index = 0;
                self.takes.clear();
                let nothing = self.proposal.as_ref().is_none_or(|p| {
                    !p.blocks
                        .iter()
                        .any(|b| matches!(b.change, model::Change::Revised | model::Change::Added))
                });
                self.notice = None;
                if nothing && self.proposal.as_ref().is_some_and(|p| p.flags.is_empty()) {
                    self.notice = Some(
                        "The reviser changed nothing. Commit keeps only your own edits.".into(),
                    );
                }
                self.view = View::Proposal;
                self.show(c);
            }
        }
    }

    fn commit(&mut self, c: &mut Context) {
        let (Some(flight), Some(proposal)) = (&self.inflight, &self.proposal) else {
            return;
        };
        if self.committing.is_some() {
            return;
        }
        let decisions: Vec<(String, Take)> = proposal
            .blocks
            .iter()
            .filter(|b| matches!(b.change, model::Change::Revised | model::Change::Added))
            .map(|b| (b.id.clone(), self.take_of(&b.id)))
            .collect();
        self.committing = c.spawn(wire::commit(&self.gateway, &flight.review_id, &decisions));
        self.notice = Some("Committing...".into());
        self.show(c);
    }

    fn commit_outcome(&mut self, c: &mut Context, out: TaskOutcome) {
        let reply = match out {
            TaskOutcome::Completed(bytes) => wire::commit_reply(&bytes),
            TaskOutcome::Failed(error) => {
                self.notice = Some(Self::fail_text(error));
                self.show(c);
                return;
            }
            TaskOutcome::Cancelled => return,
        };
        match reply {
            Some(CommitReply::Committed { sha } | CommitReply::Duplicate { sha }) => {
                let message = format!("Committed {sha} to GitHub.");
                self.finish_review(c, &message);
            }
            Some(CommitReply::Unchanged) => {
                self.finish_review(c, "Nothing to commit: the text is unchanged.");
            }
            Some(CommitReply::Stale(message)) => {
                self.notice = Some(format!(
                    "{} Discard this result and reload the section.",
                    if message.is_empty() {
                        "The section changed on GitHub.".to_owned()
                    } else {
                        message
                    }
                ));
                self.show(c);
            }
            Some(CommitReply::Rejected(message)) => {
                self.notice = Some(format!("The gateway refused the commit: {message}"));
                self.show(c);
            }
            None => {
                self.notice = Some("The gateway's answer could not be read.".into());
                self.show(c);
            }
        }
    }

    fn finish_review(&mut self, c: &mut Context, message: &str) {
        self.inflight = None;
        self.proposal = None;
        self.takes.clear();
        c.store().forget(INFLIGHT_KEY);
        self.ops.clear();
        self.save_ops(c);
        if let Some(id) = self.section_id() {
            c.store().forget(format!("sec.{id}"));
            self.current = self.current.take().map(|mut s| {
                s.rev.clear();
                s
            });
            self.fetch_section(c, &id);
        }
        let _ = self.rebuild();
        self.notice = Some(message.to_owned());
        self.view = View::Reading;
        self.check_list(c);
        self.show(c);
    }

    fn discard(&mut self, c: &mut Context) {
        self.stop_waiting(c);
        self.inflight = None;
        self.proposal = None;
        self.takes.clear();
        c.store().forget(INFLIGHT_KEY);
        self.notice = Some("Discarded the result. Your changes are still queued.".into());
        self.view = View::Queue;
        self.show(c);
    }

    // ----------------------------------------------------------- navigation

    fn go_back(&mut self, c: &mut Context) {
        if self.menu.is_some() {
            self.menu = None;
        } else if self.more_open {
            self.more_open = false;
        } else if self.row_menu.is_some() || self.queue_menu.is_some() {
            self.row_menu = None;
            self.queue_menu = None;
        } else {
            match self.view {
                View::Reading => {
                    if self.anchor.take().is_none() {
                        self.view = View::Sections;
                        self.notice = None;
                    }
                }
                View::Outline | View::Queue | View::Asking => {
                    self.range = None;
                    self.view = View::Reading;
                }
                View::Placing => self.view = View::Outline,
                View::Typing => {
                    self.entry.close();
                    self.typing = None;
                    self.view = self.typing_return;
                }
                View::Waiting | View::Proposal => {
                    self.stop_waiting(c);
                    self.view = View::Queue;
                }
                View::Setup if !self.gateway.is_empty() => self.view = View::Sections,
                _ => {}
            }
        }
        self.show(c);
    }

    fn page_by(&mut self, c: &mut Context, forward: bool) {
        let (count, index): (usize, &mut usize) = match self.view {
            View::Sections => (self.sections_screen(c).1, &mut self.list_page),
            View::Reading => {
                self.ensure_pages(c);
                (self.pages.len(), &mut self.page)
            }
            View::Outline => (self.outline_pages(c).len(), &mut self.outline_page_index),
            View::Placing => (usize::MAX, &mut self.outline_page_index),
            View::Proposal => (self.proposal_pages(c).len(), &mut self.proposal_page_index),
            View::Asking => (self.ask_pages(c).len(), &mut self.ask_page),
            _ => return,
        };
        let count = if count == usize::MAX { 64 } else { count };
        *index = if forward {
            (*index + 1).min(count.saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        self.menu = None;
        self.show(c);
    }

    fn jump_to_block(&mut self, c: &mut Context, block: usize) {
        self.pages_valid = false;
        self.ensure_pages(c);
        if let Some(page) = self
            .pages
            .iter()
            .position(|p| p.iter().any(|i| i.block() == block))
        {
            self.page = page;
        }
        self.view = View::Reading;
        self.show(c);
    }

    fn setup_key(&mut self, c: &mut Context, a: ActionId) -> bool {
        let Some(key) = self.keyboard.press(a) else {
            return false;
        };
        if matches!(key, Pressed::Submitted) {
            let text = self.keyboard.text().trim().to_owned();
            if text.starts_with("https://") {
                self.gateway = text;
                c.store()
                    .save(GATEWAY_KEY, self.gateway.clone().into_bytes());
                self.keyboard.clear();
                self.notice = None;
                self.view = View::Sections;
                self.show(c);
                self.check_list(c);
                return true;
            }
            self.notice = Some("Enter the secure https address of your gateway.".into());
        }
        self.show(c);
        true
    }
}

impl KoboApp for Drafter {
    fn on_start(&mut self, c: &mut Context) {
        c.store().load(GATEWAY_KEY);
        c.store().load(LIST_KEY);
        c.store().load(HINT_KEY);
        c.store().load(INFLIGHT_KEY);
        self.show(c);
    }

    fn on_store(&mut self, c: &mut Context, result: StoreResult) {
        let StoreResult::Loaded { key, value } = result else {
            return;
        };
        if let Some(id) = key.strip_prefix("sec.") {
            if self.opening.as_deref() == Some(id) {
                self.cached_section = Some(value.as_deref().and_then(wire::section_data));
                self.finish_open(c);
            }
        } else if let Some(id) = key.strip_prefix("ops.") {
            if self.opening.as_deref() == Some(id) {
                self.cached_ops = Some(
                    value
                        .as_deref()
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .and_then(model::ops_from_json)
                        .unwrap_or_default(),
                );
                self.finish_open(c);
            }
        } else if key == GATEWAY_KEY {
            self.gateway = value
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
        } else if key == LIST_KEY {
            if let Some(list) = value.as_deref().and_then(wire::section_list) {
                self.list = list;
            }
        } else if key == HINT_KEY {
            self.hint_seen = value.is_some();
        } else if key == INFLIGHT_KEY {
            self.inflight = value.as_deref().and_then(Inflight::decode);
            // Answers come back in request order, so this is the last of the
            // four start-up reads: the app can now say where it is.
            if self.view == View::Opening && self.opening.is_none() {
                self.view = if self.gateway.is_empty() {
                    self.keyboard = Keyboard::with_text("https://");
                    View::Setup
                } else {
                    View::Sections
                };
                self.show(c);
                if self.view == View::Sections {
                    self.check_list(c);
                }
            }
        }
    }

    fn on_text_hold(&mut self, c: &mut Context, action: ActionId, hit: TextHit) {
        if self.view == View::Reading && self.menu.is_none() && is(action, views::HOLD) {
            self.word_hold(c, hit);
        }
    }

    fn on_action(&mut self, c: &mut Context, a: ActionId) {
        if self.view == View::Typing {
            match self.entry.handle(a) {
                Some(Typing::Submitted(text)) => self.typed(c, text),
                Some(Typing::Cancelled) => self.go_back(c),
                Some(Typing::Changed) => self.show(c),
                None if a == ActionId::BACK => self.go_back(c),
                None => {}
            }
            return;
        }
        if self.view == View::Setup && self.setup_key(c, a) {
            return;
        }
        if a == ActionId::BACK {
            self.go_back(c);
            return;
        }
        if self.menu.is_some() && self.menu_action(c, a) {
            return;
        }
        if is(a, views::HOLD) {
            if self.view == View::Reading {
                self.notice =
                    Some("Hold a word in a paragraph. Tables and rules are in the outline.".into());
                self.show(c);
            }
            return;
        }
        if is(a, MORE) {
            self.more_open = !self.more_open;
            self.show(c);
            return;
        }
        self.more_open = false;
        match self.view {
            View::Sections => self.sections_action(c, a),
            View::Reading => self.reading_action(c, a),
            View::Outline => self.outline_action(c, a),
            View::Placing => self.placing_action(c, a),
            View::Asking => self.asking_action(c, a),
            View::Queue => self.queue_action(c, a),
            View::Waiting => {
                if is(a, STOP) {
                    self.stop_waiting(c);
                    self.view = View::Queue;
                    self.show(c);
                }
            }
            View::Proposal => self.proposal_action(c, a),
            View::Opening | View::Setup | View::Typing => {}
        }
    }

    fn on_task(&mut self, c: &mut Context, id: TaskId, out: TaskOutcome) {
        if self.clock.on_task(c, id, &out) {
            self.poll(c);
            if self.view == View::Waiting {
                self.show(c);
            }
            return;
        }
        if self.fetching_list == Some(id) {
            self.fetching_list = None;
            self.list_outcome(c, out);
        } else if self.fetching_section == Some(id) {
            self.fetching_section = None;
            self.section_outcome(c, out);
        } else if self.posting == Some(id) {
            self.posting = None;
            self.review_outcome(c, out);
        } else if self.polling == Some(id) {
            self.polling = None;
            self.job_outcome(c, out);
        } else if self.committing == Some(id) {
            self.committing = None;
            self.commit_outcome(c, out);
        }
    }

    fn on_foreground(&mut self, c: &mut Context) {
        self.show(c);
    }
}

// Per-view actions live apart from the trait impl to keep it readable.
impl Drafter {
    fn menu_action(&mut self, c: &mut Context, a: ActionId) -> bool {
        match self.menu.clone() {
            Some(Menu::Word { .. }) => self.word_menu_action(c, a),
            Some(Menu::Block { block }) => self.block_action(c, a, block),
            Some(Menu::Send) => {
                if is(a, YES) {
                    self.confirmed_locked = true;
                    self.menu = None;
                    self.send(c);
                } else if is(a, NO) {
                    self.menu = None;
                    self.show(c);
                } else {
                    return false;
                }
                true
            }
            Some(Menu::Clear) => {
                if is(a, YES) {
                    self.ops.clear();
                    let _ = self.rebuild();
                    self.save_ops(c);
                    self.notice = Some("Cleared the queue.".into());
                }
                if is(a, YES) || is(a, NO) {
                    self.menu = None;
                    self.show(c);
                    return true;
                }
                false
            }
            None => false,
        }
    }

    fn list_outcome(&mut self, c: &mut Context, out: TaskOutcome) {
        match out {
            TaskOutcome::Completed(bytes) => match wire::section_list(&bytes) {
                Some(list) => {
                    self.list = list;
                    self.notice = None;
                    c.store().save(LIST_KEY, bytes);
                }
                None => {
                    self.notice =
                        Some("The gateway answered with something other than sections.".into());
                }
            },
            TaskOutcome::Failed(error) => {
                self.notice = Some(format!(
                    "{} The saved list is still here.",
                    Self::fail_text(error)
                ));
            }
            TaskOutcome::Cancelled => {}
        }
        if self.view == View::Sections {
            self.show(c);
        }
    }

    fn section_outcome(&mut self, c: &mut Context, out: TaskOutcome) {
        match out {
            TaskOutcome::Completed(bytes) => {
                if let Some(data) = wire::section_data(&bytes) {
                    let fresh = self.current.as_ref().is_none_or(|s| s.rev != data.rev);
                    if fresh && (self.ops.is_empty() || self.current.is_none()) {
                        c.store().save(format!("sec.{}", data.id), bytes);
                    }
                    self.adopt_section(c, data, fresh);
                } else {
                    self.notice =
                        Some("The gateway answered with something other than a section.".into());
                    self.show(c);
                }
            }
            TaskOutcome::Failed(error) => {
                self.notice = Some(Self::fail_text(error));
                if self.view == View::Opening {
                    self.view = View::Sections;
                }
                self.show(c);
            }
            TaskOutcome::Cancelled => {}
        }
    }

    fn sections_action(&mut self, c: &mut Context, a: ActionId) {
        if is(a, REFRESH) {
            self.notice = None;
            self.check_list(c);
            self.show(c);
        } else if is(a, GATEWAY) {
            self.keyboard = Keyboard::with_text(self.gateway.clone());
            self.view = View::Setup;
            self.show(c);
        } else if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        } else if let Some(index) = index_of(a, "sec", self.list.items.len()) {
            self.open_section(c, index);
        }
    }

    fn reading_action(&mut self, c: &mut Context, a: ActionId) {
        if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        } else if is(a, OUTLINE) {
            self.notice = None;
            self.outline_page_index = 0;
            self.view = View::Outline;
            self.show(c);
        } else if is(a, QUEUE) {
            self.notice = None;
            self.queue_page_index = 0;
            self.view = View::Queue;
            self.show(c);
        } else if is(a, UNDO) {
            self.undo(c);
            self.show(c);
        } else if is(a, RELOAD) {
            if self.ops.is_empty() {
                if let Some(id) = self.section_id() {
                    self.notice = Some("Reloading from GitHub.".into());
                    self.fetch_section(c, &id);
                }
            } else {
                self.notice = Some("Clear the queue first, or its changes would be lost.".into());
            }
            self.show(c);
        } else if is(a, LIST) {
            self.view = View::Sections;
            self.notice = None;
            self.show(c);
        }
    }

    fn outline_action(&mut self, c: &mut Context, a: ActionId) {
        let count = self.work.blocks.len();
        if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        } else if let Some(i) = index_of(a, "o", count) {
            self.jump_to_block(c, i);
        } else if let Some(i) = index_of(a, "om", count) {
            self.row_menu = if self.row_menu == Some(i) {
                None
            } else {
                Some(i)
            };
            self.show(c);
        } else if let Some(open) = self.row_menu {
            if self.block_action(c, a, open) {
                self.row_menu = None;
            }
        }
    }

    fn placing_action(&mut self, c: &mut Context, a: ActionId) {
        let Some(moving) = self.placing.clone() else {
            return;
        };
        if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
            return;
        }
        let after = if is(a, "place.top") {
            Some(None)
        } else {
            index_of(a, "place", self.work.blocks.len())
                .and_then(|i| self.work.blocks.get(i))
                .map(|b| Some(b.id.clone()))
        };
        if let Some(after) = after {
            self.placing = None;
            self.push_op(
                c,
                Op::Move {
                    block: moving,
                    after,
                },
                "Moved.",
            );
            self.view = View::Outline;
            self.after_structure(c);
        }
    }

    fn asking_action(&mut self, c: &mut Context, a: ActionId) {
        if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        } else if let Some(ask) = Ask::ALL
            .into_iter()
            .find(|ask| is(a, &format!("ask.{}", ask.name())))
        {
            self.mark_with(c, ask);
        }
    }

    fn queue_action(&mut self, c: &mut Context, a: ActionId) {
        let count = self.ops.len();
        if is(a, SEND) {
            self.send(c);
        } else if is(a, CHECK) {
            self.notice = Some("Checking the gateway.".into());
            self.poll(c);
            self.show(c);
        } else if is(a, REVIEW) {
            self.view = View::Proposal;
            self.show(c);
        } else if is(a, UNDO) {
            self.undo(c);
            self.show(c);
        } else if is(a, CLEAR) {
            self.menu = Some(Menu::Clear);
            self.show(c);
        } else if let Some(i) = index_of(a, "q", count).or_else(|| index_of(a, "qm", count)) {
            self.queue_menu = if self.queue_menu == Some(i) {
                None
            } else {
                Some(i)
            };
            self.show(c);
        } else if is(a, "q-remove") {
            if let Some(i) = self.queue_menu.take() {
                let removed = self.ops.remove(i);
                if self.rebuild().is_ok() {
                    self.save_ops(c);
                    self.notice = Some("Removed.".into());
                } else {
                    self.ops.insert(i, removed);
                    let _ = self.rebuild();
                    self.notice =
                        Some("Other changes depend on that one. Undo from the end instead.".into());
                }
            }
            self.show(c);
        } else if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        }
    }

    fn proposal_action(&mut self, c: &mut Context, a: ActionId) {
        let count = self.proposal.as_ref().map_or(0, |p| p.blocks.len());
        if is(a, COMMIT) {
            self.commit(c);
        } else if is(a, DISCARD) {
            self.discard(c);
        } else if is(a, PREV) || is(a, NEXT) {
            self.page_by(c, is(a, NEXT));
        } else if let Some(i) = index_of(a, "tk", count) {
            self.set_take(c, i, Take::Proposal);
        } else if let Some(i) = index_of(a, "tm", count) {
            self.set_take(c, i, Take::Designer);
        }
    }

    fn set_take(&mut self, c: &mut Context, index: usize, take: Take) {
        let Some(id) = self
            .proposal
            .as_ref()
            .and_then(|p| p.blocks.get(index))
            .map(|b| b.id.clone())
        else {
            return;
        };
        self.takes.retain(|(block, _)| *block != id);
        self.takes.push((id, take));
        self.show(c);
    }
}

#[cfg(test)]
mod tests;

fn main() -> ExitCode {
    kobo_sdk::run("drafter", Drafter::default())
        .map_or_else(|_| ExitCode::FAILURE, |()| ExitCode::SUCCESS)
}
