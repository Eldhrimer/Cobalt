//! Every screen the drafter draws. Each is a pure function of the app's state.

use crate::model::{clip, Ask, Block, Change, Kind, Working};
use crate::pages::{self, Item};
use crate::wire::{Proposal, Take};
use crate::{Drafter, Menu};

/// What a proposal page is made of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PItem {
    /// Flags, counts and lint: the first page.
    Summary,
    /// A revised or added block's label and its keep/leave choice.
    Head(usize),
    /// A run of that block's text or rows.
    Body(Item),
}
use kobo_sdk::{BannerLevel, Chrome, Context, Glyph, Screen, ScreenBuilder, Space};

// Action names. One place, so a typo is a compile error rather than a dead button.
pub const HOLD: &str = "hold";
pub const PREV: &str = "prev-page";
pub const NEXT: &str = "next-page";
pub const MORE: &str = "more";
pub const OUTLINE: &str = "outline";
pub const QUEUE: &str = "queue";
pub const REFRESH: &str = "refresh";
pub const UNDO: &str = "undo";
pub const CLEAR: &str = "clear";
pub const RELOAD: &str = "reload";
pub const LIST: &str = "list";
pub const GATEWAY: &str = "gateway-setup";
pub const SEND: &str = "send";
pub const CHECK: &str = "check-result";
pub const REVIEW: &str = "review-result";
pub const STOP: &str = "stop-waiting";
pub const COMMIT: &str = "commit";
pub const DISCARD: &str = "discard";
pub const YES: &str = "confirm-yes";
pub const NO: &str = "confirm-no";

impl Drafter {
    fn measure(screen: &Screen, c: &Context) -> bool {
        !screen
            .diagnostics(&c.metrics(), &Chrome::measuring(true))
            .has_errors()
    }

    /// Greedy pages of row indices: add rows until the page stops fitting.
    pub(crate) fn index_pages(
        count: usize,
        c: &Context,
        build: impl Fn(&[usize]) -> Screen,
    ) -> Vec<Vec<usize>> {
        let mut pages = Vec::new();
        let mut page: Vec<usize> = Vec::new();
        for index in 0..count {
            page.push(index);
            if page.len() > 1 && !Self::measure(&build(&page), c) {
                page.pop();
                pages.push(std::mem::take(&mut page));
                page.push(index);
            }
        }
        if !page.is_empty() {
            pages.push(page);
        }
        pages
    }

    // --------------------------------------------------------------- setup

    pub(crate) fn setup_screen(&self) -> Screen {
        let mut s = ScreenBuilder::new("drafter-setup")
            .top_bar("Drafter")
            .section("Connect a gateway")
            .secondary("Drafter reads your book from a gateway you run. On your computer:")
            .command(
                "kobo drafter login --gateway https://drafter.example.net --token-file token.txt",
            );
        if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Attention, notice);
        }
        s.field(
            "gateway-url",
            self.keyboard.text(),
            "https://drafter.example.net",
        )
        .keyboard(&self.keyboard, "Save gateway")
        .build()
    }

    // ------------------------------------------------------------ sections

    fn sections_page(&self, indices: &[usize], page: usize, total: usize) -> Screen {
        let mut s = ScreenBuilder::new("drafter-sections")
            .page_turns(PREV, NEXT)
            .top_bar("Drafter")
            .top_bar_action(REFRESH, "Check")
            .top_bar_overflow(MORE, self.more_open, [(GATEWAY, "Change gateway")]);
        if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Attention, notice);
        }
        if self.list.items.is_empty() {
            let (title, summary) = if self.fetching_list.is_some() {
                ("Opening the draft", "Asking the gateway for its sections.")
            } else {
                (
                    "No sections yet",
                    "Check again when the gateway can reach your repository.",
                )
            };
            return s.splash(Some(Glyph::Book), title, summary).build();
        }
        let items = &self.list.items;
        s = s.rows_with_trailing(indices.iter().map(|&i| {
            let item = &items[i];
            (
                format!("sec.{i}"),
                item.title.clone(),
                format!("{}, {} words", item.chapter, item.words),
                Glyph::Note,
                item.status.clone(),
            )
        }));
        s.secondary(format!(
            "{} - {} of {}",
            self.list.draft,
            page + 1,
            total.max(1)
        ))
        .spacer(Space::Small)
        .action_bar([(PREV, "Previous"), (NEXT, "Next")])
        .build()
    }

    pub(crate) fn sections_screen(&self, c: &Context) -> (Screen, usize) {
        let pages = Self::index_pages(self.list.items.len(), c, |p| self.sections_page(p, 0, 1));
        let page = self.list_page.min(pages.len().saturating_sub(1));
        let screen = self.sections_page(
            pages.get(page).map_or(&[], Vec::as_slice),
            page,
            pages.len(),
        );
        (screen, pages.len())
    }

    // ------------------------------------------------------------- reading

    pub(crate) fn reading_page(&self, items: &[Item], page: usize, total: usize) -> Screen {
        let title = self
            .current
            .as_ref()
            .map_or("Section", |s| s.title.as_str());
        let mut s = ScreenBuilder::new("drafter-read")
            .top_bar(title)
            .top_bar_overflow(
                MORE,
                self.more_open,
                [
                    (UNDO, "Undo last change"),
                    (RELOAD, "Reload from GitHub"),
                    (LIST, "All sections"),
                ],
            )
            .hold(HOLD)
            .owns_back(true);
        if total > 1 {
            s = s.page_turns(PREV, NEXT).page_position(
                u16::try_from(page + 1).unwrap_or(u16::MAX),
                u16::try_from(total).unwrap_or(u16::MAX),
            );
        }
        if let Some(anchor) = &self.anchor {
            s = s.banner(
                BannerLevel::Info,
                format!(
                    "Highlight starts at \"{}\". Hold its last word.",
                    clip(&anchor.word, 24)
                ),
            );
        } else if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Info, notice);
        } else if !self.hint_seen {
            s = s.secondary("Hold a word to highlight, split or move its paragraph.");
        }
        if items.is_empty() {
            s = s.empty_state("This section has no text yet.");
        }
        for item in items {
            s = pages::render(s, &self.work, *item, true);
        }
        s = s.action_bar([(OUTLINE, "Outline".to_owned()), (QUEUE, self.queue_label())]);
        if let Some(menu) = &self.menu {
            s = self.menu_overlay(s, menu);
        }
        s.build()
    }

    pub(crate) fn queue_label(&self) -> String {
        if self.ops.is_empty() {
            "Queue".to_owned()
        } else {
            format!("Queue ({})", self.ops.len())
        }
    }

    fn menu_overlay(&self, s: ScreenBuilder, menu: &Menu) -> ScreenBuilder {
        match menu {
            Menu::Word {
                block,
                part,
                start,
                end,
            } => {
                let Some(b) = self.work.blocks.get(*block) else {
                    return s;
                };
                let text = &b.parts[*part].text;
                let word = text.get(*start..*end).unwrap_or_default();
                let anchored = self.anchor.is_some();
                s.modal(format!("{}: {}", b.kind.noun(), clip(word, 22)), |m| {
                    let m = m
                        .button("w.sentence", "Highlight this sentence")
                        .button("w.paragraph", "Highlight this paragraph")
                        .button(
                            if anchored { "w.end" } else { "w.start" },
                            if anchored {
                                "End highlight here"
                            } else {
                                "Start highlight here"
                            },
                        );
                    let m = if b.kind == Kind::Para && *start > 0 {
                        m.button("w.split", "Split before this word")
                    } else {
                        m
                    };
                    m.button("w.block", "Block options")
                })
            }
            Menu::Block { block } => {
                let Some(b) = self.work.blocks.get(*block) else {
                    return s;
                };
                let above_is_para = *block > 0 && self.work.blocks[*block - 1].kind == Kind::Para;
                s.modal(format!("{} options", b.kind.noun()), |m| {
                    let mut m = m.button("b.move", "Move it");
                    if b.kind == Kind::Para && above_is_para {
                        m = m.button("b.merge", "Merge into the block above");
                    }
                    if matches!(b.kind, Kind::Para | Kind::Heading) {
                        m = m.button("b.retype", "Retype it");
                    }
                    m.button("b.note", "Add a note")
                        .button("b.delete", "Delete it")
                })
            }
            // Confirmations are whole screens (see `confirm_screen`), never a
            // dialog laid over a busy list.
            Menu::Send | Menu::Clear => s,
        }
    }

    fn confirm_screen(menu: &Menu) -> Screen {
        let (title, question, yes) = match menu {
            Menu::Send => (
                "This section is Locked",
                "Locked sections are finished. Send these changes for revision anyway?",
                "Send anyway",
            ),
            _ => (
                "Clear the queue?",
                "Every queued change to this section is dropped.",
                "Clear it",
            ),
        };
        ScreenBuilder::new("drafter-confirm")
            .top_bar("Queue")
            .owns_back(true)
            .confirmation(
                title,
                question,
                kobo_sdk::DialogAction::new(YES, yes),
                kobo_sdk::DialogAction::new(NO, "Cancel"),
            )
            .build()
    }

    pub(crate) fn reading_screen(&self) -> Screen {
        let page = self.page.min(self.pages.len().saturating_sub(1));
        self.reading_page(
            self.pages.get(page).map_or(&[], Vec::as_slice),
            page,
            self.pages.len(),
        )
        .with_own_back(true)
    }

    pub(crate) fn paginate_reading(&self, c: &Context) -> Vec<Vec<Item>> {
        let pages = pages::paginate(&self.work, |page| {
            Self::measure(&self.reading_page(page, 0, 2), c)
        });
        if pages.is_empty() {
            vec![Vec::new()]
        } else {
            pages
        }
    }

    // ------------------------------------------------------------- outline

    fn block_summary(&self, index: usize) -> String {
        let block = &self.work.blocks[index];
        let marks = self.work.marks_in(&block.id).count();
        let base = self
            .current
            .as_ref()
            .and_then(|s| s.blocks.iter().find(|b| b.id == block.id));
        let state = match base {
            None => "added by hand",
            Some(b) if b.parts != block.parts => "edited",
            Some(_) => "",
        };
        let size = match block.kind {
            Kind::Table => format!("{} rows", block.rows.len()),
            Kind::Hr => String::new(),
            _ => format!(
                "{} words",
                block
                    .parts
                    .iter()
                    .map(|p| p.text.split_whitespace().count())
                    .sum::<usize>()
            ),
        };
        let mut parts = vec![size];
        if marks > 0 {
            parts.push(format!("{marks} highlighted"));
        }
        if !state.is_empty() {
            parts.push(state.to_owned());
        }
        parts.retain(|p| !p.is_empty());
        parts.join(", ")
    }

    fn outline_page(&self, c: &Context, indices: &[usize]) -> Screen {
        let mut s = ScreenBuilder::new("drafter-outline")
            .top_bar("Outline")
            .owns_back(true);
        if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Info, notice);
        }
        s = s.rows_with_menu(indices.iter().map(|&i| {
            let block = &self.work.blocks[i];
            (
                format!("o.{i}"),
                c.clamped_row_with_menu(
                    &format!("{}: {}", block.kind.noun(), block.snippet(90)),
                    2,
                    true,
                ),
                self.block_summary(i),
                u16::try_from(i + 1).unwrap_or(u16::MAX),
                format!("om.{i}"),
            )
        }));
        if let Some(open) = self.row_menu.filter(|o| indices.contains(o)) {
            let block = &self.work.blocks[open];
            let mut items = vec![("ov.move", "Move", Glyph::Next)];
            if block.kind == Kind::Para && open > 0 && self.work.blocks[open - 1].kind == Kind::Para
            {
                items.push(("ov.merge", "Merge up", Glyph::Previous));
            }
            if matches!(block.kind, Kind::Para | Kind::Heading) {
                items.push(("ov.retype", "Retype", Glyph::Note));
            }
            items.extend([
                ("ov.note", "Note", Glyph::Chat),
                ("ov.add", "Write a paragraph after", Glyph::Plus),
                ("ov.want", "Ask for a paragraph after", Glyph::Search),
                ("ov.delete", "Delete", Glyph::Trash),
            ]);
            s = s.row_overflow(format!("om.{open}"), true, items);
        }
        s.action_bar([(PREV, "Previous"), (NEXT, "Next")]).build()
    }

    pub(crate) fn outline_pages(&self, c: &Context) -> Vec<Vec<usize>> {
        Self::index_pages(self.work.blocks.len(), c, |p| self.outline_page(c, p))
    }

    pub(crate) fn outline_screen(&self, c: &Context) -> Screen {
        let pages = self.outline_pages(c);
        let page = self.outline_page_index.min(pages.len().saturating_sub(1));
        self.outline_page(c, pages.get(page).map_or(&[], Vec::as_slice))
            .with_own_back(true)
    }

    // ------------------------------------------------------------- placing

    fn placing_page(&self, c: &Context, moving: &Block, indices: &[usize]) -> Screen {
        let mut s = ScreenBuilder::new("drafter-place")
            .top_bar("Move")
            .owns_back(true)
            .section("Moving")
            .secondary(c.clamped_row(&moving.snippet(120), 2, true));
        let rows = indices.iter().map(|&i| {
            if i == usize::MAX {
                (
                    "place.top".to_owned(),
                    "Start of the section".to_owned(),
                    "Put it first.".to_owned(),
                    Glyph::Previous,
                )
            } else {
                let b = &self.work.blocks[i];
                (
                    format!("place.{i}"),
                    c.clamped_row(&format!("{}: {}", b.kind.noun(), b.snippet(90)), 2, true),
                    "Put it after this block.".to_owned(),
                    Glyph::Next,
                )
            }
        });
        s = s.rows(rows);
        s.action_bar([(PREV, "Previous"), (NEXT, "Next")]).build()
    }

    pub(crate) fn placing_targets(&self, moving: &str) -> Vec<usize> {
        std::iter::once(usize::MAX)
            .chain(
                self.work
                    .blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.id != moving)
                    .map(|(i, _)| i),
            )
            .collect()
    }

    pub(crate) fn placing_screen(&self, c: &Context, moving: &str) -> Screen {
        let Some(block) = self.work.blocks.iter().find(|b| b.id == moving) else {
            return self.outline_screen(c);
        };
        let targets = self.placing_targets(moving);
        let pages = Self::index_pages(targets.len(), c, |p| {
            let picked: Vec<usize> = p.iter().map(|&n| targets[n]).collect();
            self.placing_page(c, block, &picked)
        });
        let page = self.outline_page_index.min(pages.len().saturating_sub(1));
        let picked: Vec<usize> = pages
            .get(page)
            .map_or_else(Vec::new, |p| p.iter().map(|&n| targets[n]).collect());
        self.placing_page(c, block, &picked)
    }

    // --------------------------------------------------------------- asking

    fn asking_page(&self, c: &Context, indices: &[usize]) -> Screen {
        let range = self.range.as_ref();
        ScreenBuilder::new("drafter-ask")
            .top_bar("What should change?")
            .owns_back(true)
            .section("The passage")
            .quote(
                1,
                c.clamped_row(&range.map(|r| r.text.clone()).unwrap_or_default(), 3, true),
            )
            .rows(indices.iter().map(|&i| {
                let ask = Ask::ALL[i];
                (
                    format!("ask.{}", ask.name()),
                    ask.label(),
                    ask.detail(),
                    Glyph::Chat,
                )
            }))
            .action_bar([(PREV, "Previous"), (NEXT, "Next")])
            .build()
    }

    pub(crate) fn ask_pages(&self, c: &Context) -> Vec<Vec<usize>> {
        Self::index_pages(Ask::ALL.len(), c, |p| self.asking_page(c, p))
    }

    pub(crate) fn asking_screen(&self, c: &Context) -> Screen {
        let pages = self.ask_pages(c);
        let page = self.ask_page.min(pages.len().saturating_sub(1));
        self.asking_page(c, pages.get(page).map_or(&[], Vec::as_slice))
            .with_own_back(true)
    }

    pub(crate) fn typing_screen(&self) -> Screen {
        let (title, prompt) = self
            .typing
            .as_ref()
            .map_or(("Note", "Note"), crate::Typed::prompt);
        ScreenBuilder::new("drafter-typing")
            .top_bar(title)
            .owns_back(true)
            .text_entry(&self.entry, prompt, "Save")
            .build()
    }

    // ---------------------------------------------------------------- queue

    fn queue_page(&self, c: &Context, indices: &[usize]) -> Screen {
        let mut s = ScreenBuilder::new("drafter-queue")
            .top_bar("Queue")
            .owns_back(true);
        if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Attention, notice);
        }
        if self.ops.is_empty() {
            s = s.splash(
                Some(Glyph::Note),
                "Nothing queued",
                "Hold a word to highlight it, or open the outline to move and delete blocks.",
            );
        } else {
            s = s.rows_with_menu(indices.iter().map(|&i| {
                (
                    format!("q.{i}"),
                    c.clamped_row_with_menu(&self.ops[i].describe(), 2, true),
                    if self.ops[i].is_instruction() {
                        "Sent to the reviser"
                    } else {
                        "Applied as you made it"
                    },
                    Glyph::Note,
                    format!("qm.{i}"),
                )
            }));
            if let Some(open) = self.queue_menu.filter(|o| indices.contains(o)) {
                s = s.row_overflow(
                    format!("qm.{open}"),
                    true,
                    [("q-remove", "Remove this", Glyph::Trash)],
                );
            }
        }
        s = match (&self.inflight, &self.proposal) {
            (Some(_), Some(_)) => s.primary_button(REVIEW, "Review the result"),
            (Some(_), None) => s.primary_button(CHECK, "Check the result"),
            (None, _) if self.ops.is_empty() => s.disabled_button(SEND, "Send for revision"),
            (None, _) => s.primary_button(SEND, "Send for revision"),
        };
        if !self.ops.is_empty() {
            s = s.buttons([(UNDO, "Undo last"), (CLEAR, "Clear all")]);
        }
        s.build()
    }

    pub(crate) fn queue_screen(&self, c: &Context) -> Screen {
        if let Some(menu @ (Menu::Send | Menu::Clear)) = &self.menu {
            return Self::confirm_screen(menu).with_own_back(true);
        }
        let pages = Self::index_pages(self.ops.len(), c, |p| self.queue_page(c, p));
        let page = self.queue_page_index.min(pages.len().saturating_sub(1));
        self.queue_page(c, pages.get(page).map_or(&[], Vec::as_slice))
            .with_own_back(true)
    }

    // -------------------------------------------------------------- waiting

    pub(crate) fn waiting_screen(&self) -> Screen {
        let label = if self.waiting.is_empty() {
            "Waiting for the gateway"
        } else {
            &self.waiting
        };
        let mut s = ScreenBuilder::new("drafter-waiting")
            .top_bar("Revising")
            .owns_back(true)
            .activity(label, None)
            .cancellable(STOP, "Stop waiting");
        let words = self.clock.waited_words();
        if !words.is_empty() {
            s = s.secondary(words);
        }
        s.secondary("The result waits for you in the queue if you leave.")
            .build()
    }

    // ------------------------------------------------------------- proposal

    pub(crate) fn take_of(&self, id: &str) -> Take {
        self.takes
            .iter()
            .find(|(block, _)| block == id)
            .map_or(Take::Proposal, |(_, take)| *take)
    }

    fn changed(proposal: &Proposal) -> Vec<usize> {
        proposal
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.change, Change::Revised | Change::Added))
            .map(|(i, _)| i)
            .collect()
    }

    fn proposal_page(
        &self,
        proposal: &Proposal,
        items: &[PItem],
        work: &Working,
        page: usize,
        total: usize,
    ) -> Screen {
        let mut s = ScreenBuilder::new("drafter-proposal")
            .top_bar("Revisions")
            .owns_back(true);
        if total > 1 {
            s = s.page_turns(PREV, NEXT).page_position(
                u16::try_from(page + 1).unwrap_or(u16::MAX),
                u16::try_from(total).unwrap_or(u16::MAX),
            );
        }
        for item in items {
            s = match *item {
                PItem::Summary => self.proposal_summary_nodes(s, proposal, total),
                PItem::Head(i) => {
                    let block = &proposal.blocks[i];
                    let mine = self.take_of(&block.id) == Take::Designer;
                    let added = block.change == Change::Added;
                    let label = if added { "Added" } else { "Revised" };
                    let mut s = s.section(if block.why.is_empty() {
                        label.to_owned()
                    } else {
                        format!("{label}: {}", clip(&block.why, 60))
                    });
                    s = s.chips([
                        (
                            format!("tk.{i}"),
                            if added { "Keep it" } else { "Use revision" },
                            !mine,
                        ),
                        (
                            format!("tm.{i}"),
                            if added { "Leave it out" } else { "Keep mine" },
                            mine,
                        ),
                    ]);
                    if let (true, Some(before)) = (mine, &block.before) {
                        s = s.text(format!("Yours: {}", clip(&before.snippet(120), 120)));
                    }
                    s
                }
                PItem::Body(body) => pages::render(s, work, body, false),
            };
        }
        s.bottom_action(COMMIT, "Commit to GitHub").build()
    }

    fn proposal_summary_nodes(
        &self,
        mut s: ScreenBuilder,
        proposal: &Proposal,
        total: usize,
    ) -> ScreenBuilder {
        if let Some(notice) = &self.notice {
            s = s.banner(BannerLevel::Attention, notice);
        }
        for flag in proposal.flags.iter().take(4) {
            s = s.banner(BannerLevel::Info, clip(&flag.issue, 160));
        }
        if proposal.flags.len() > 4 {
            s = s.text(format!(
                "{} more notes from the reviser.",
                proposal.flags.len() - 4
            ));
        }
        s = s.text(Self::proposal_summary(proposal));
        for found in proposal.lint.new.iter().take(3) {
            s = s.text(format!("New: {}", clip(found, 100)));
        }
        if proposal.dropped > 0 {
            s = s.text(format!(
                "{} highlights were on text you deleted and were left out.",
                proposal.dropped
            ));
        }
        if total > 1 {
            s = s.secondary("Tap the right of the page to read each revision.");
        }
        s
    }

    fn proposal_summary(proposal: &Proposal) -> String {
        let revised = proposal
            .blocks
            .iter()
            .filter(|b| b.change == Change::Revised)
            .count();
        let added = proposal
            .blocks
            .iter()
            .filter(|b| b.change == Change::Added)
            .count();
        let yours = proposal
            .blocks
            .iter()
            .filter(|b| b.change == Change::Yours)
            .count();
        let lint = if proposal.lint.available {
            format!(
                "Lint: {} violations, {} warnings.",
                proposal.lint.violations, proposal.lint.warnings
            )
        } else {
            "Lint was not available.".to_owned()
        };
        let mine = if yours > 0 {
            format!(" {yours} blocks are your own edits.")
        } else {
            String::new()
        };
        format!("{revised} revised, {added} added.{mine} {lint}")
    }

    pub(crate) fn proposal_pages(&self, c: &Context) -> Vec<Vec<PItem>> {
        let Some(proposal) = &self.proposal else {
            return vec![vec![PItem::Summary]];
        };
        let work = Working {
            blocks: proposal.blocks.clone(),
            ..Working::default()
        };
        let fits =
            |page: &[PItem]| Self::measure(&self.proposal_page(proposal, page, &work, 1, 3), c);
        // The summary heads the first page and shares it with the first
        // revision when both fit; otherwise it stands alone.
        let mut pages: Vec<Vec<PItem>> = Vec::new();
        let mut page: Vec<PItem> = vec![PItem::Summary];
        for &index in &Self::changed(proposal) {
            let mut atoms: std::collections::VecDeque<Item> = pages::atoms_of(&work, index).into();
            let mut headed = false;
            while let Some(atom) = atoms.pop_front() {
                let lead: Vec<PItem> = if headed {
                    Vec::new()
                } else {
                    vec![PItem::Head(index)]
                };
                let mut candidate = page.clone();
                candidate.extend(lead.iter().copied());
                candidate.push(PItem::Body(atom));
                if fits(&candidate) {
                    page = candidate;
                    headed = true;
                } else if !page.is_empty() {
                    pages.push(std::mem::take(&mut page));
                    atoms.push_front(atom);
                } else {
                    let (head, tail) = pages::split(&work, atom, &mut |alone| {
                        let mut alone_page = lead.clone();
                        alone_page.push(PItem::Body(alone));
                        fits(&alone_page)
                    });
                    let mut shown = lead;
                    shown.push(PItem::Body(head));
                    pages.push(shown);
                    headed = true;
                    if let Some(tail) = tail {
                        atoms.push_front(tail);
                    }
                }
            }
        }
        if !page.is_empty() {
            pages.push(page);
        }
        pages
    }

    pub(crate) fn proposal_screen(&self, c: &Context) -> Screen {
        let Some(proposal) = &self.proposal else {
            return self.queue_screen(c);
        };
        let work = Working {
            blocks: proposal.blocks.clone(),
            ..Working::default()
        };
        let pages = self.proposal_pages(c);
        let page = self.proposal_page_index.min(pages.len().saturating_sub(1));
        self.proposal_page(proposal, &pages[page], &work, page, pages.len())
            .with_own_back(true)
    }
}
