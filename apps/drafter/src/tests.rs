//! Whole-app tests: the real state machine against a scripted gateway.
use super::*;
use kobo_sdk::{AppRunner, Chrome, Command, DisplayMetrics, StoreRequest, Task};
use kobo_ui::{TextScale, CLARA_BW_METRICS};
use std::collections::BTreeMap;

const GATEWAY: &str = "https://drafter.example/wu";
const SECTION: &str = "a1b2c3d4e5f6";

const LIST: &str = r#"{"draft":"v0.6","items":[
 {"id":"a1b2c3d4e5f6","file":"02/rules.md","chapter":"Rules of Play","title":"The Lantern Roll","status":"Approved","blocks":7,"words":160},
 {"id":"b1b2c3d4e5f6","file":"02/rules.md","chapter":"Rules of Play","title":"Skill","status":"Locked","blocks":1,"words":20}]}"#;

fn section_json(rev: &str) -> String {
    format!(
        r#"{{"id":"{SECTION}","title":"The Lantern Roll","file":"02/rules.md","rev":"{rev}","status":"Approved","blocks":[
 {{"id":"p1","kind":"para","text":"Most of what happens in the cellar just happens. You say what your character does, and the story moves on. The roll enters only when the fiction has not settled the outcome.","spans":[[0,4,"b"]]}},
 {{"id":"p2","kind":"para","text":"Pass. Your total meets or beats the target, and no Slip turns up.","spans":[[0,4,"b"]]}},
 {{"id":"p3","kind":"para","text":"Fail. Your total falls short of the target. The narrator adds 1 to the Gloom.","spans":[[0,5,"b"]]}},
 {{"id":"t1","kind":"table","rows":[{{"header":true,"cells":["Move","Cues","What it covers"]}},{{"header":false,"cells":["CLIMB","Scramble","Getting up what is in the way."]}},{{"header":false,"cells":["HIDE","Duck","Staying out of the light."]}}]}},
 {{"id":"c1","kind":"callout","label":"example","title":"","parts":[{{"text":"A scout with a d8 in CLIMB squares up against a crumbling stair.","spans":[]}}]}},
 {{"id":"l1","kind":"list","parts":[{{"text":"First item","spans":[],"depth":0,"ordered":false}},{{"text":"Second item","spans":[],"depth":0,"ordered":false}}]}},
 {{"id":"m1","kind":"marker","text":"DECISION NEEDED: count"}}]}}"#
    )
}

fn proposal_json(job_state: &str) -> String {
    format!(
        r#"{{"state":"{job_state}","message":"","proposal":{{"title":"The Lantern Roll","blocks":[
 {{"id":"p1","kind":"para","text":"Most of what happens in the cellar just happens. The roll enters only when the fiction has not settled the outcome.","spans":[],"change":"revised","why":"Fixture: tighten.","before":{{"id":"p1","kind":"para","text":"Most of what happens in the cellar just happens. You say what your character does, and the story moves on.","spans":[]}}}},
 {{"id":"p2","kind":"para","text":"Pass. Your total meets or beats the target, and no Slip turns up.","spans":[[0,4,"b"]],"change":"same"}},
 {{"id":"ai1","kind":"para","text":"A new paragraph.","spans":[],"change":"added","why":"Drafted from your note."}}],
 "flags":[{{"block":"p1","issue":"Check: the skill rule is stated twice."}}],"dropped":0,
 "lint":{{"available":true,"violations":0,"warnings":2,"new":[]}}}}}}"#
    )
}

struct Rig {
    runner: AppRunner<Drafter>,
    metrics: DisplayMetrics,
    spawned: Vec<(TaskId, Task)>,
    store: BTreeMap<String, Vec<u8>>,
    forgotten: Vec<String>,
    screen: Option<Screen>,
}

impl Rig {
    fn new(metrics: DisplayMetrics) -> Self {
        Self {
            runner: AppRunner::with_metrics(Drafter::default(), metrics),
            metrics,
            spawned: Vec::new(),
            store: BTreeMap::new(),
            forgotten: Vec::new(),
            screen: None,
        }
    }

    fn standard() -> Self {
        Self::new(CLARA_BW_METRICS)
    }

    fn absorb(&mut self, commands: Vec<Command>) {
        let mut loads = Vec::new();
        for command in commands {
            match command {
                Command::SetScreen(screen) => self.screen = Some(screen),
                Command::Spawn { task, work } => self.spawned.push((task, work)),
                Command::Store(StoreRequest::Save { key, value }) => {
                    self.store.insert(key, value);
                }
                Command::Store(StoreRequest::Forget { key }) => {
                    self.store.remove(&key);
                    self.forgotten.push(key);
                }
                Command::Store(StoreRequest::Load { key }) => loads.push(key),
                _ => {}
            }
        }
        for key in loads {
            let value = self.store.get(&key).cloned();
            let more = self.runner.store_result(StoreResult::Loaded { key, value });
            self.absorb(more);
        }
    }

    fn start(&mut self) {
        let commands = self.runner.start();
        self.absorb(commands);
    }

    fn act(&mut self, name: &str) {
        let commands = self.runner.action(action_id(name));
        self.absorb(commands);
    }

    fn back(&mut self) {
        let commands = self.runner.action(ActionId::BACK);
        self.absorb(commands);
    }

    fn hold(&mut self, hit: TextHit) {
        let commands = self.runner.text_hold(action_id(views::HOLD), hit);
        self.absorb(commands);
    }

    fn app(&self) -> &Drafter {
        self.runner.app()
    }

    /// Answers the newest spawned task whose URL ends with `suffix`.
    fn answer(&mut self, suffix: &str, body: &str) {
        let (task, _) = self
            .spawned
            .iter()
            .rev()
            .find(|(_, work)| match work {
                Task::Fetch { url, .. } | Task::Post { url, .. } => url.ends_with(suffix),
                _ => false,
            })
            .unwrap_or_else(|| panic!("no task for {suffix}: {:?}", self.urls()))
            .clone();
        let commands = self
            .runner
            .task_outcome(task, TaskOutcome::Completed(body.as_bytes().to_vec()));
        self.absorb(commands);
    }

    fn fail(&mut self, suffix: &str, error: TaskError) {
        let task = self
            .spawned
            .iter()
            .rev()
            .find(|(_, w)| matches!(w, Task::Fetch { url, .. } | Task::Post { url, .. } if url.ends_with(suffix)))
            .unwrap()
            .0;
        let commands = self.runner.task_outcome(task, TaskOutcome::Failed(error));
        self.absorb(commands);
    }

    fn urls(&self) -> Vec<String> {
        self.spawned
            .iter()
            .filter_map(|(_, w)| match w {
                Task::Fetch { url, .. } | Task::Post { url, .. } => Some(url.clone()),
                _ => None,
            })
            .collect()
    }

    fn posted(&self, suffix: &str) -> Option<String> {
        self.spawned.iter().rev().find_map(|(_, w)| match w {
            Task::Post { url, body, .. } if url.ends_with(suffix) => Some(body.clone()),
            _ => None,
        })
    }

    /// Boots with a saved gateway, opens the first section from the gateway.
    fn open_section(&mut self) {
        self.store
            .insert(GATEWAY_KEY.into(), GATEWAY.as_bytes().to_vec());
        self.start();
        self.answer("/sections", LIST);
        self.act("sec.0");
        self.answer(&format!("/sections/{SECTION}"), &section_json("rev1"));
    }

    /// Everything the screen says: laid-out lines, plus rich-text paragraphs
    /// (which the layout leaves unlisted) in the screen and its overlay.
    fn texts(&self) -> Vec<String> {
        let Some(screen) = &self.screen else {
            return Vec::new();
        };
        let mut out: Vec<String> = screen
            .layout_with(&self.metrics, &Chrome::measuring(true))
            .nodes
            .iter()
            .flat_map(|n| n.text_lines.iter().cloned())
            .collect();
        let overlay = screen.overlay.iter().flat_map(|o| o.nodes.iter());
        for node in screen.nodes.iter().chain(overlay) {
            if let kobo_ui::Node::RichText { text, .. } = node {
                out.push(text.clone());
            }
        }
        out
    }

    /// Delivers the heartbeat's nap, which is what makes the app poll again.
    fn tick(&mut self) {
        let (task, _) = self
            .spawned
            .iter()
            .rev()
            .find(|(_, w)| matches!(w, Task::Sleep { .. }))
            .expect("a heartbeat is running")
            .clone();
        let commands = self
            .runner
            .task_outcome(task, TaskOutcome::Completed(Vec::new()));
        self.absorb(commands);
    }

    fn says(&self, needle: &str) -> bool {
        self.texts().join(" ").contains(needle)
    }

    fn view(&self) -> View {
        self.app().view
    }
}

fn hit(app: &Drafter, block: usize, word: &str) -> TextHit {
    let text = &app.work.blocks[block].parts[0].text;
    let start = text
        .find(word)
        .unwrap_or_else(|| panic!("{word} not in {text}"));
    TextHit {
        context: pages::pack(block, 0, 0),
        start: u32::try_from(start).unwrap(),
        end: u32::try_from(start + word.len()).unwrap(),
    }
}

fn assert_fits(screen: &Screen, metrics: DisplayMetrics, what: &str) {
    let chrome = Chrome::for_screen(screen, false, Chrome::measuring(true).status);
    let errors: Vec<_> = screen
        .diagnostics(&metrics, &chrome)
        .issues
        .into_iter()
        .filter(|i| i.severity == kobo_sdk::DiagnosticSeverity::Error)
        .collect();
    assert!(errors.is_empty(), "{what} at {metrics:?}: {errors:#?}");
}

fn panels() -> impl Iterator<Item = DisplayMetrics> {
    [(1072, 1448, 300), (1264, 1680, 300), (1404, 1872, 227)]
        .into_iter()
        .flat_map(|(w, h, ppi)| {
            TextScale::STEPS
                .into_iter()
                .map(move |text_scale| DisplayMetrics {
                    width: w,
                    height: h,
                    pixels_per_inch: ppi,
                    text_scale,
                })
        })
}

// ------------------------------------------------------------------ tests

#[test]
fn a_new_install_asks_for_a_gateway_and_a_configured_one_lists_sections() {
    let mut rig = Rig::standard();
    rig.start();
    assert_eq!(rig.view(), View::Setup);
    assert!(rig.says("kobo drafter login"));

    let mut rig = Rig::standard();
    rig.store
        .insert(GATEWAY_KEY.into(), GATEWAY.as_bytes().to_vec());
    rig.start();
    assert_eq!(rig.view(), View::Sections);
    assert!(rig.urls().iter().any(|u| u.ends_with("/wu/sections")));
    rig.answer("/sections", LIST);
    assert!(rig.says("The Lantern Roll"));
    assert!(rig.says("Locked"));
    assert!(
        rig.store.contains_key(LIST_KEY),
        "the list is kept for offline"
    );
}

#[test]
fn the_gateway_address_must_be_https() {
    let mut rig = Rig::standard();
    rig.start();
    for ch in "http://nope".chars() {
        let _ = ch;
    }
    rig.runner.app_mut().keyboard = Keyboard::with_text("http://nope");
    let commands = rig.runner.action(action_id("kb.enter"));
    rig.absorb(commands);
    assert_eq!(rig.view(), View::Setup);
    assert!(rig.says("secure https address"));
    rig.runner.app_mut().keyboard = Keyboard::with_text(GATEWAY);
    let commands = rig.runner.action(action_id("kb.enter"));
    rig.absorb(commands);
    assert_eq!(rig.view(), View::Sections);
    assert_eq!(
        rig.store.get(GATEWAY_KEY).map(Vec::as_slice),
        Some(GATEWAY.as_bytes())
    );
}

#[test]
fn opening_a_section_renders_every_block_and_keeps_a_copy() {
    let mut rig = Rig::standard();
    rig.open_section();
    assert_eq!(rig.view(), View::Reading);
    assert!(rig.says("Most of what happens"));
    assert!(rig.store.contains_key(&format!("sec.{SECTION}")));
    let all: Vec<Item> = rig.app().pages.iter().flatten().copied().collect();
    let blocks: std::collections::BTreeSet<usize> = all.iter().map(|i| i.block()).collect();
    assert_eq!(blocks.len(), 7, "every block is on some page: {all:?}");
}

#[test]
fn every_page_fits_every_panel_and_text_size() {
    for metrics in panels() {
        let mut rig = Rig::new(metrics);
        rig.open_section();
        let total = rig.app().pages.len();
        assert!(total >= 1);
        for page in 0..total {
            rig.runner.app_mut().page = page;
            let screen = rig.runner.app().reading_screen();
            assert_fits(&screen, metrics, &format!("reading page {page}"));
            let layout = screen.layout_with(
                &metrics,
                &Chrome::for_screen(&screen, false, Chrome::measuring(true).status),
            );
            let outline = layout
                .rect_of_action(action_id(views::OUTLINE))
                .expect("outline reachable");
            assert!(outline.height >= metrics.touch_target_minimum());
        }
        // No words lost to pagination.
        let mut shown = String::new();
        for item in rig.app().pages.iter().flatten() {
            if let Item::Text {
                block,
                part,
                from,
                to,
            } = *item
            {
                shown.push_str(&rig.app().work.blocks[block].parts[part].text[from..to]);
                shown.push(' ');
            }
        }
        for word in "Most of what happens in the cellar just happens roll enters stair First item"
            .split(' ')
        {
            assert!(shown.contains(word), "{word} missing at {metrics:?}");
        }
    }
}

#[test]
fn holding_a_word_opens_a_menu_and_a_sentence_highlight_is_queued() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    assert!(matches!(rig.app().menu, Some(Menu::Word { .. })));
    assert!(rig.says("Highlight this sentence"));
    assert!(rig.app().hint_seen);
    rig.act("w.sentence");
    assert_eq!(rig.view(), View::Asking);
    assert!(rig.says("You say what your character does, and the story moves on."));
    rig.act("ask.tighten");
    assert_eq!(rig.view(), View::Reading);
    assert_eq!(rig.app().ops.len(), 1);
    let Op::Mark { text, ask, .. } = &rig.app().ops[0] else {
        panic!("mark")
    };
    assert_eq!(
        text,
        "You say what your character does, and the story moves on."
    );
    assert_eq!(*ask, Ask::Tighten);
    assert_eq!(rig.app().work.marks.len(), 1);
    assert!(
        rig.store.contains_key(&format!("ops.{SECTION}")),
        "queued work is saved"
    );
    assert!(rig.says("Queue (1)"));
}

#[test]
fn a_range_is_two_holds_and_must_stay_in_one_paragraph() {
    let mut rig = Rig::standard();
    rig.open_section();
    let first = hit(rig.app(), 0, "You say");
    rig.hold(first);
    rig.act("w.start");
    assert!(rig.says("Highlight starts at"));
    let last = hit(rig.app(), 0, "moves");
    rig.hold(last);
    assert!(rig.says("End highlight here"));
    rig.act("w.end");
    assert_eq!(rig.view(), View::Asking);
    let range = rig.app().range.clone().unwrap();
    assert!(
        range.text.starts_with("You say") && range.text.ends_with("moves"),
        "{}",
        range.text
    );
    rig.act("ask.cut");
    assert!(matches!(rig.app().ops[0], Op::Mark { ask: Ask::Cut, .. }));

    // Crossing into another paragraph is refused, not guessed at.
    let from = hit(rig.app(), 0, "Most");
    rig.hold(from);
    rig.act("w.start");
    let elsewhere = hit(rig.app(), 1, "total");
    rig.hold(elsewhere);
    rig.act("w.end");
    assert_eq!(rig.view(), View::Reading);
    assert!(rig.says("stays inside one paragraph"));
    assert_eq!(rig.app().ops.len(), 1);
}

#[test]
fn a_custom_ask_takes_a_typed_note() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 1, "Slip");
    rig.hold(word);
    rig.act("w.paragraph");
    rig.act("ask.custom");
    assert_eq!(rig.view(), View::Typing);
    for key in ["kb.r0c0", "kb.r0c1"] {
        rig.act(key);
    }
    rig.act("kb.enter");
    assert_eq!(rig.view(), View::Reading);
    let Op::Mark { ask, note, .. } = &rig.app().ops[0] else {
        panic!("mark")
    };
    assert_eq!(*ask, Ask::Custom);
    assert_eq!(note.len(), 2);
}

#[test]
fn split_merge_delete_and_undo_edit_the_working_copy_and_the_saved_queue() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "The roll enters");
    rig.hold(word);
    rig.act("w.split");
    assert_eq!(rig.app().work.blocks.len(), 8);
    assert!(rig.app().work.blocks[1].parts[0]
        .text
        .starts_with("The roll enters"));

    // Merge it straight back through the block menu.
    let again = hit(rig.app(), 1, "roll");
    rig.hold(again);
    rig.act("w.block");
    assert!(rig.says("Merge into the block above"));
    rig.act("b.merge");
    assert_eq!(rig.app().work.blocks.len(), 7);

    let word = hit(rig.app(), 2, "Fail");
    rig.hold(word);
    rig.act("w.block");
    rig.act("b.delete");
    assert_eq!(rig.app().work.blocks.len(), 6);
    assert_eq!(rig.app().ops.len(), 3);

    rig.act("more");
    rig.act("undo");
    assert_eq!(rig.app().work.blocks.len(), 7);
    assert_eq!(rig.app().ops.len(), 2);
    let saved = String::from_utf8(rig.store[&format!("ops.{SECTION}")].clone()).unwrap();
    assert_eq!(model::ops_from_json(&saved).unwrap().len(), 2);
}

#[test]
fn a_refused_operation_is_taken_back_and_explained() {
    let mut rig = Rig::standard();
    rig.open_section();
    let first = hit(rig.app(), 0, "Most");
    rig.hold(first);
    rig.act("w.block");
    assert!(
        !rig.says("Merge into the block above"),
        "the first block has nothing above"
    );
    rig.back();
    rig.runner.app_mut().push_op(
        &mut kobo_sdk::Context::default(),
        Op::Merge { block: "p1".into() },
        "x",
    );
    assert!(rig.app().ops.is_empty());
    assert!(rig
        .app()
        .notice
        .as_deref()
        .unwrap()
        .starts_with("Not possible"));
}

#[test]
fn moving_a_block_is_two_taps_through_the_outline() {
    let mut rig = Rig::standard();
    rig.open_section();
    rig.act("outline");
    assert_eq!(rig.view(), View::Outline);
    assert!(rig.says("Table:"));
    rig.act("next-page");
    assert!(
        rig.says("Marker:"),
        "later blocks are on the next outline page"
    );
    rig.act("prev-page");
    rig.act("om.3");
    assert!(rig.says("Move"));
    rig.act("ov.move");
    assert_eq!(rig.view(), View::Placing);
    assert!(rig.says("Start of the section"));
    rig.act("place.0");
    assert_eq!(rig.view(), View::Outline);
    assert_eq!(rig.app().work.blocks[0].id, "p1");
    assert_eq!(rig.app().work.blocks[1].id, "t1");
    assert!(
        matches!(&rig.app().ops[0], Op::Move { block, after: Some(a) } if block == "t1" && a == "p1")
    );

    // Tapping a row jumps to that block in the reading view.
    rig.act("o.1");
    assert_eq!(rig.view(), View::Reading);
    let page = &rig.app().pages[rig.app().page];
    assert!(page.iter().any(|i| i.block() == 1));
}

#[test]
fn typed_blocks_come_from_the_outline_menu() {
    let mut rig = Rig::standard();
    rig.open_section();
    rig.act("outline");
    rig.act("om.0");
    rig.act("ov.add");
    assert_eq!(rig.view(), View::Typing);
    rig.act("kb.r0c0");
    rig.act("kb.enter");
    assert_eq!(rig.view(), View::Outline);
    assert_eq!(rig.app().work.blocks.len(), 8);
    assert_eq!(rig.app().work.blocks[1].id, "d1");
    rig.act("om.0");
    rig.act("ov.want");
    rig.act("kb.r0c0");
    rig.act("kb.enter");
    assert!(matches!(rig.app().ops.last(), Some(Op::Want { after: Some(a), .. }) if a == "p1"));
}

#[test]
fn a_restart_restores_the_queue_against_the_saved_section() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.plainer");
    let store = rig.store.clone();

    let mut rig = Rig::standard();
    rig.store = store;
    rig.start();
    rig.act("sec.0");
    assert_eq!(
        rig.view(),
        View::Reading,
        "the saved copy opens without the network"
    );
    assert_eq!(rig.app().ops.len(), 1);
    assert_eq!(rig.app().work.marks.len(), 1);
    assert!(rig
        .urls()
        .iter()
        .any(|u| u.ends_with(&format!("/sections/{SECTION}"))));
}

#[test]
fn sending_polls_the_job_and_shows_the_revisions() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.tighten");
    rig.act("queue");
    assert_eq!(rig.view(), View::Queue);
    rig.act("send");
    assert_eq!(rig.view(), View::Waiting);

    let body = rig.posted("/reviews").expect("review posted");
    let parsed = kobo_json::parse(&body).unwrap();
    assert_eq!(parsed.get("section_id").unwrap().as_str(), Some(SECTION));
    assert_eq!(parsed.get("rev").unwrap().as_str(), Some("rev1"));
    assert_eq!(parsed.get("review_id").unwrap().as_str().unwrap().len(), 64);
    assert_eq!(parsed.get("ops").unwrap().as_array().unwrap().len(), 1);

    rig.answer(
        "/reviews",
        r#"{"status":"accepted","job_id":"0123456789abcdef0123456789abcdef"}"#,
    );
    assert!(rig.store.contains_key(INFLIGHT_KEY));
    rig.answer(
        "/jobs/0123456789abcdef0123456789abcdef",
        r#"{"state":"running","message":"Revising"}"#,
    );
    assert!(rig.says("Revising"));
    rig.tick();
    rig.answer(
        "/jobs/0123456789abcdef0123456789abcdef",
        &proposal_json("done"),
    );
    assert_eq!(rig.view(), View::Proposal);
    assert!(
        rig.says("skill rule is stated twice"),
        "flags lead the summary page"
    );
    assert!(rig.says("1 revised, 1 added"));
    assert!(rig.says("0 violations, 2 warnings"));
    assert!(
        rig.says("Fixture: tighten."),
        "a short summary shares the first page"
    );
    assert!(rig.says("Use revision"));
}

#[test]
fn choosing_and_committing_sends_decisions_and_clears_the_queue() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.tighten");
    rig.act("queue");
    rig.act("send");
    rig.answer(
        "/reviews",
        r#"{"status":"accepted","job_id":"0123456789abcdef0123456789abcdef"}"#,
    );
    rig.answer(
        "/jobs/0123456789abcdef0123456789abcdef",
        &proposal_json("done"),
    );

    rig.act("tm.0");
    assert!(rig.says("Yours:"));
    rig.act("tm.2");
    rig.act("tk.0");
    rig.act("commit");
    let body = rig.posted("/commits").expect("commit posted");
    let parsed = kobo_json::parse(&body).unwrap();
    let decisions = parsed.get("decisions").unwrap().as_array().unwrap();
    let takes: Vec<_> = decisions
        .iter()
        .map(|d| {
            (
                d.get("block").unwrap().as_str().unwrap(),
                d.get("take").unwrap().as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(takes, [("p1", "proposal"), ("ai1", "designer")]);

    rig.answer(
        "/commits",
        r#"{"status":"committed","sha":"abc1234567","file":"x"}"#,
    );
    assert_eq!(rig.view(), View::Reading);
    assert!(rig.says("Committed abc1234567 to GitHub."));
    assert!(rig.app().ops.is_empty());
    assert!(rig.app().inflight.is_none());
    assert!(rig.forgotten.contains(&INFLIGHT_KEY.to_owned()));
    assert!(rig.forgotten.contains(&format!("ops.{SECTION}")));
    assert!(
        rig.urls()
            .iter()
            .filter(|u| u.ends_with(&format!("/sections/{SECTION}")))
            .count()
            >= 2
    );
}

#[test]
fn a_stale_or_failed_review_returns_to_the_queue_with_the_reason_and_the_changes() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.tighten");
    rig.act("queue");
    rig.act("send");
    rig.answer(
        "/reviews",
        r#"{"status":"stale","message":"The section changed since you opened it."}"#,
    );
    assert_eq!(rig.view(), View::Queue);
    assert!(rig.says("The section changed since you opened it."));
    assert_eq!(rig.app().ops.len(), 1, "nothing is lost");
    assert!(rig.app().inflight.is_none());

    rig.act("send");
    rig.answer(
        "/reviews",
        r#"{"status":"accepted","job_id":"0123456789abcdef0123456789abcdef"}"#,
    );
    rig.answer(
        "/jobs/0123456789abcdef0123456789abcdef",
        r#"{"state":"failed","message":"Claude could not be reached: down"}"#,
    );
    assert_eq!(rig.view(), View::Queue);
    assert!(rig.says("Claude could not be reached"));
}

#[test]
fn network_failures_are_explained_and_a_missing_login_names_the_fix() {
    let mut rig = Rig::standard();
    rig.store
        .insert(GATEWAY_KEY.into(), GATEWAY.as_bytes().to_vec());
    rig.start();
    rig.fail("/sections", TaskError::NoCredential);
    assert!(rig.says("kobo drafter login"));
    rig.act("refresh");
    rig.fail("/sections", TaskError::Unreachable);
    assert!(rig.says("Couldn't reach the gateway"));
}

#[test]
fn a_locked_section_asks_before_it_sends() {
    let mut rig = Rig::standard();
    rig.open_section();
    rig.runner.app_mut().current.as_mut().unwrap().status = "Locked".into();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.tighten");
    rig.act("queue");
    rig.act("send");
    assert!(rig.says("This section is Locked"));
    assert!(rig.posted("/reviews").is_none());
    rig.act("confirm-yes");
    assert!(rig.posted("/reviews").is_some());
}

#[test]
fn a_changed_section_keeps_the_readers_base_while_changes_are_queued() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    rig.act("w.sentence");
    rig.act("ask.tighten");
    rig.act("more");
    rig.act("reload");
    assert!(rig.says("Clear the queue first"));
    let store = rig.store.clone();

    // Next day: the saved copy and queue open at once; GitHub has moved on.
    let mut rig = Rig::standard();
    rig.store = store;
    rig.start();
    rig.act("sec.0");
    assert_eq!(rig.view(), View::Reading);
    rig.answer(&format!("/sections/{SECTION}"), &section_json("rev2"));
    assert_eq!(
        rig.app().current.as_ref().unwrap().rev,
        "rev1",
        "the queue still fits rev1"
    );
    assert!(rig.says("changed on GitHub"));
    assert_eq!(rig.app().ops.len(), 1);
}

#[test]
fn back_walks_out_one_layer_at_a_time() {
    let mut rig = Rig::standard();
    rig.open_section();
    let word = hit(rig.app(), 0, "say");
    rig.hold(word);
    assert!(rig.app().menu.is_some());
    rig.back();
    assert!(rig.app().menu.is_none());
    assert_eq!(rig.view(), View::Reading);
    rig.act("outline");
    rig.act("om.0");
    rig.back();
    assert!(rig.app().row_menu.is_none());
    assert_eq!(rig.view(), View::Outline);
    rig.back();
    assert_eq!(rig.view(), View::Reading);
    rig.back();
    assert_eq!(rig.view(), View::Sections);
}

#[test]
fn every_screen_fits_every_panel_and_text_size() {
    for metrics in panels() {
        let mut rig = Rig::new(metrics);
        rig.open_section();
        let word = hit(rig.app(), 0, "say");
        rig.hold(word);
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "word menu");
        rig.act("w.block");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "block menu");
        rig.back();
        rig.act("w.sentence");
        let _ = rig.app();
        rig.hold(word);
        rig.act("w.sentence");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "ask");
        rig.act("ask.concrete");
        rig.act("outline");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "outline");
        rig.act("om.3");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "outline menu");
        rig.act("ov.move");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "placing");
        rig.back();
        rig.back();
        rig.act("queue");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "queue");
        rig.act("send");
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "waiting");
        rig.answer(
            "/reviews",
            r#"{"status":"accepted","job_id":"0123456789abcdef0123456789abcdef"}"#,
        );
        rig.answer(
            "/jobs/0123456789abcdef0123456789abcdef",
            &proposal_json("done"),
        );
        assert_fits(rig.screen.as_ref().unwrap(), metrics, "proposal");
        rig.act("tm.0");
        rig.act("tm.2");
        let context = rig.runner.context();
        let total = rig.app().proposal_pages(&context).len();
        assert!(total >= 1);
        for page in 0..total {
            rig.runner.app_mut().proposal_page_index = page;
            let screen = rig.runner.app().proposal_screen(&context);
            assert_fits(
                &screen,
                metrics,
                &format!("proposal page {page}, mine chosen"),
            );
        }
    }
}

#[test]
fn a_long_paragraph_splits_across_pages_and_still_takes_holds_anywhere_in_it() {
    let mut rig = Rig::standard();
    rig.store
        .insert(GATEWAY_KEY.into(), GATEWAY.as_bytes().to_vec());
    rig.start();
    rig.answer("/sections", LIST);
    rig.act("sec.0");
    let sentence = "The attempt fails and the fiction keeps moving, just not your way. ";
    let long = sentence.repeat(40);
    let json = format!(
        r#"{{"id":"{SECTION}","title":"Long","file":"f.md","rev":"r","status":"","blocks":[{{"id":"p1","kind":"para","text":"{}","spans":[]}}]}}"#,
        long.trim_end()
    );
    rig.answer(&format!("/sections/{SECTION}"), &json);
    let pages = rig.app().pages.len();
    assert!(pages >= 2, "{pages}");
    rig.act("next-page");
    let Item::Text { from, .. } = rig.app().pages[1][0] else {
        panic!("text")
    };
    assert!(from > 0);
    let at = from + 10;
    let hit = TextHit {
        context: pages::pack(0, 0, 0),
        start: u32::try_from(at).unwrap(),
        end: u32::try_from(at + 3).unwrap(),
    };
    rig.hold(hit);
    let Some(Menu::Word { start, .. }) = rig.app().menu.clone() else {
        panic!("menu")
    };
    assert_eq!(start, at);
}

#[test]
fn list_markers_do_not_shift_the_words_a_hold_resolves() {
    let mut rig = Rig::standard();
    rig.open_section();
    let list = rig
        .app()
        .work
        .blocks
        .iter()
        .position(|b| b.id == "l1")
        .unwrap();
    let prefix = "- ".len();
    // The runtime reports offsets in the displayed text, marker included.
    let word = "Second".len();
    let hit = TextHit {
        context: pages::pack(list, 1, prefix),
        start: u32::try_from(prefix * 2).unwrap(),
        end: u32::try_from(prefix * 2 + word).unwrap(),
    };
    let _ = hit;
    let text = &rig.app().work.blocks[list].parts[1].text;
    assert_eq!(&text[0..word], "Second");
    let shown = TextHit {
        context: pages::pack(list, 1, prefix),
        start: 4,
        end: 10,
    };
    rig.hold(shown);
    let Some(Menu::Word {
        part, start, end, ..
    }) = rig.app().menu.clone()
    else {
        panic!("menu")
    };
    assert_eq!((part, start, end), (1, 0, 6));
}

#[test]
fn no_screen_draws_a_character_the_face_cannot() {
    let mut rig = Rig::standard();
    rig.open_section();
    {
        let screen = rig.screen.clone().unwrap();
        let issues = screen
            .diagnostics(&rig.metrics, &Chrome::measuring(true))
            .issues;
        assert!(
            !issues
                .iter()
                .any(|i| format!("{i:?}").to_lowercase().contains("glyph")),
            "{issues:#?}"
        );
    }
}

#[test]
fn a_tall_table_fills_the_first_page_instead_of_stranding_the_paragraph_above_it() {
    let mut rig = Rig::standard();
    rig.store
        .insert(GATEWAY_KEY.into(), GATEWAY.as_bytes().to_vec());
    rig.start();
    rig.answer("/sections", LIST);
    rig.act("sec.0");
    let rows: String = (0..11)
        .map(|n| {
            format!(
                r#"{{"header":{},"cells":["ACTION{n}","What this action covers in the fiction, with an example or two of it."]}}"#,
                n == 0
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"id":"{SECTION}","title":"Long","file":"f.md","rev":"r","status":"","blocks":[
 {{"id":"p1","kind":"para","text":"Everything a character attempts goes through one of six moves.","spans":[]}},
 {{"id":"t1","kind":"table","rows":[{rows}]}},
 {{"id":"p2","kind":"para","text":"Roll two dice at that skill and add them.","spans":[]}}]}}"#
    );
    rig.answer(&format!("/sections/{SECTION}"), &json);
    let pages = &rig.app().pages;
    assert!(
        pages[0]
            .iter()
            .any(|item| matches!(item, Item::Rows { .. })),
        "the first page carries the start of the table: {pages:?}"
    );
    assert!(
        pages.len() <= 3,
        "eleven table rows are not six pages: {pages:?}"
    );
    let rows_shown: usize = pages
        .iter()
        .flatten()
        .map(|item| match item {
            Item::Rows { from, to, .. } => to - from,
            _ => 0,
        })
        .sum();
    assert_eq!(rows_shown, 11, "no row is lost between pages");
}
