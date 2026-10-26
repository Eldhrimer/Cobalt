//! The reviewed gateway contract. A gateway serves five routes under its base
//! address, all behind one bearer token the runtime holds:
//!
//! * `GET  /sections`        the draft's sections, with statuses
//! * `GET  /sections/{id}`   one section as blocks, with a revision token
//! * `POST /reviews`         the designer's operations for one section
//! * `GET  /jobs/{id}`       a review's progress, then its proposal
//! * `POST /commits`         which revisions to keep
//!
//! `kobo-policy` pins the same routes and body shapes, so a compromised app
//! cannot use the token for anything else.
use crate::model::{self, Block, Op};
use kobo_json::{ObjectBuilder, Value};
use kobo_sdk::{Credential, Task};

pub const SECRET: &str = "wu-drafter";
const LIST_MAX: u32 = 256 * 1024;
const SECTION_MAX: u32 = 512 * 1024;
const JOB_MAX: u32 = 768 * 1024;
const REPLY_MAX: u32 = 4096;

pub fn endpoint(gateway: &str, route: &str) -> String {
    format!(
        "{}/{}",
        gateway.trim_end_matches('/'),
        route.trim_start_matches('/')
    )
}

fn fetch(gateway: &str, route: &str, max_bytes: u32) -> Task {
    Task::Fetch {
        url: endpoint(gateway, route),
        offset: 0,
        max_bytes,
        credential: Some(Credential::bearer(SECRET)),
        headers: Vec::new(),
    }
}

fn post(gateway: &str, route: &str, body: &Value) -> Task {
    Task::Post {
        url: endpoint(gateway, route),
        body: body.to_json(),
        content_type: "application/json".to_owned(),
        credential: Some(Credential::bearer(SECRET)),
        headers: Vec::new(),
        max_bytes: REPLY_MAX,
    }
}

pub fn sections(gateway: &str) -> Task {
    fetch(gateway, "/sections", LIST_MAX)
}

pub fn section(gateway: &str, id: &str) -> Task {
    fetch(gateway, &format!("/sections/{id}"), SECTION_MAX)
}

pub fn job(gateway: &str, id: &str) -> Task {
    fetch(gateway, &format!("/jobs/{id}"), JOB_MAX)
}

/// Identity of a review: the same operations on the same revision of the same
/// section are the same review, so a retried send cannot start a second one.
pub fn review_id(gateway: &str, section: &str, rev: &str, ops: &[Op]) -> String {
    kobo_net::sha256::hex_digest(
        format!(
            "drafter-review-v1\n{gateway}\n{section}\n{rev}\n{}",
            model::ops_to_json(ops)
        )
        .as_bytes(),
    )
}

pub fn review(gateway: &str, id: &str, section: &str, rev: &str, ops: &[Op]) -> Task {
    let body = ObjectBuilder::new()
        .set("review_id", id)
        .set("section_id", section)
        .set("rev", rev)
        .set("ops", Value::Array(ops.iter().map(Op::to_value).collect()))
        .build();
    post(gateway, "/reviews", &body)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Take {
    Proposal,
    Designer,
}

pub fn commit(gateway: &str, id: &str, decisions: &[(String, Take)]) -> Task {
    let list = decisions
        .iter()
        .map(|(block, take)| {
            ObjectBuilder::new()
                .set("block", block.as_str())
                .set(
                    "take",
                    if *take == Take::Proposal {
                        "proposal"
                    } else {
                        "designer"
                    },
                )
                .build()
        })
        .collect();
    let body = ObjectBuilder::new()
        .set("review_id", id)
        .set("decisions", Value::Array(list))
        .build();
    post(gateway, "/commits", &body)
}

// ------------------------------------------------------------------ replies

fn parse(bytes: &[u8]) -> Option<Value> {
    kobo_json::parse(std::str::from_utf8(bytes).ok()?).ok()
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionInfo {
    pub id: String,
    pub file: String,
    pub chapter: String,
    pub title: String,
    pub status: String,
    pub blocks: usize,
    pub words: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SectionList {
    pub draft: String,
    pub items: Vec<SectionInfo>,
}

pub fn section_list(bytes: &[u8]) -> Option<SectionList> {
    let root = parse(bytes)?;
    let mut items = Vec::new();
    for item in root.get("items")?.as_array()? {
        let count = |key: &str| usize::try_from(item.get(key)?.as_i64()?).ok();
        items.push(SectionInfo {
            id: item.get("id")?.as_str()?.to_owned(),
            file: text(item, "file"),
            chapter: text(item, "chapter"),
            title: item.get("title")?.as_str()?.to_owned(),
            status: text(item, "status"),
            blocks: count("blocks").unwrap_or(0),
            words: count("words").unwrap_or(0),
        });
    }
    Some(SectionList {
        draft: text(&root, "draft"),
        items,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionData {
    pub id: String,
    pub title: String,
    pub file: String,
    pub rev: String,
    pub status: String,
    pub blocks: Vec<Block>,
}

pub fn section_data(bytes: &[u8]) -> Option<SectionData> {
    let root = parse(bytes)?;
    Some(SectionData {
        id: root.get("id")?.as_str()?.to_owned(),
        title: root.get("title")?.as_str()?.to_owned(),
        file: text(&root, "file"),
        rev: root.get("rev")?.as_str()?.to_owned(),
        status: text(&root, "status"),
        blocks: model::blocks_from(root.get("blocks")?)?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewReply {
    Accepted(String),
    Duplicate(String),
    Stale(String),
    Rejected(String),
}

pub fn review_reply(bytes: &[u8]) -> Option<ReviewReply> {
    let root = parse(bytes)?;
    let message = text(&root, "message");
    Some(match root.get("status")?.as_str()? {
        "accepted" => ReviewReply::Accepted(root.get("job_id")?.as_str()?.to_owned()),
        "duplicate" => ReviewReply::Duplicate(root.get("job_id")?.as_str()?.to_owned()),
        "stale" => ReviewReply::Stale(message),
        "rejected" => ReviewReply::Rejected(message),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Lint {
    pub available: bool,
    pub violations: usize,
    pub warnings: usize,
    pub new: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Flag {
    pub block: String,
    pub issue: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub title: String,
    pub blocks: Vec<Block>,
    pub flags: Vec<Flag>,
    pub dropped: usize,
    pub lint: Lint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    pub state: JobState,
    pub message: String,
    pub proposal: Option<Proposal>,
    pub committed: bool,
}

pub fn job_reply(bytes: &[u8]) -> Option<Job> {
    let root = parse(bytes)?;
    let state = match root.get("state")?.as_str()? {
        "queued" => JobState::Queued,
        "running" => JobState::Running,
        "done" => JobState::Done,
        "failed" => JobState::Failed,
        _ => return None,
    };
    let proposal = match root.get("proposal") {
        Some(p) => Some(proposal_from(p)?),
        None if state == JobState::Done => return None,
        None => None,
    };
    Some(Job {
        state,
        message: text(&root, "message"),
        proposal,
        committed: root.get("committed").is_some(),
    })
}

fn proposal_from(root: &Value) -> Option<Proposal> {
    let count = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    };
    let lint = root.get("lint");
    Some(Proposal {
        title: text(root, "title"),
        blocks: model::blocks_from(root.get("blocks")?)?,
        flags: root
            .get("flags")
            .and_then(Value::as_array)
            .unwrap_or_default()
            .iter()
            .map(|f| Flag {
                block: text(f, "block"),
                issue: text(f, "issue"),
            })
            .collect(),
        dropped: count(root, "dropped"),
        lint: lint.map_or_else(Lint::default, |l| Lint {
            available: l.get("available").and_then(Value::as_bool).unwrap_or(false),
            violations: count(l, "violations"),
            warnings: count(l, "warnings"),
            new: l
                .get("new")
                .and_then(Value::as_array)
                .unwrap_or_default()
                .iter()
                .map(|n| format!("{}: {}", text(n, "kind").to_lowercase(), text(n, "message")))
                .collect(),
        }),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitReply {
    Committed { sha: String },
    Duplicate { sha: String },
    Unchanged,
    Stale(String),
    Rejected(String),
}

pub fn commit_reply(bytes: &[u8]) -> Option<CommitReply> {
    let root = parse(bytes)?;
    let message = text(&root, "message");
    Some(match root.get("status")?.as_str()? {
        "committed" => CommitReply::Committed {
            sha: text(&root, "sha"),
        },
        "duplicate" => CommitReply::Duplicate {
            sha: text(&root, "sha"),
        },
        "unchanged" => CommitReply::Unchanged,
        "stale" => CommitReply::Stale(message),
        "rejected" | "missing" => CommitReply::Rejected(message),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ask;

    const GATEWAY: &str = "https://drafter.example/wu";

    #[test]
    fn requests_name_the_secret_and_never_carry_it() {
        for task in [
            sections(GATEWAY),
            section(GATEWAY, "abc"),
            job(GATEWAY, "d"),
        ] {
            let Task::Fetch {
                credential: Some(secret),
                url,
                ..
            } = task
            else {
                panic!("fetch")
            };
            assert_eq!(secret.secret, SECRET);
            assert!(!url.contains(SECRET));
            assert!(url.starts_with("https://drafter.example/wu/"));
        }
    }

    #[test]
    fn a_review_body_is_built_not_concatenated() {
        let ops = vec![Op::Mark {
            block: "a".into(),
            part: 0,
            start: 1,
            end: 5,
            text: "say \"hi\"\nthere".into(),
            ask: Ask::Tighten,
            note: String::new(),
        }];
        let id = review_id(GATEWAY, "0123456789ab", "rev1", &ops);
        let Task::Post {
            body,
            url,
            content_type,
            ..
        } = review(GATEWAY, &id, "0123456789ab", "rev1", &ops)
        else {
            panic!("post")
        };
        assert!(url.ends_with("/wu/reviews"));
        assert_eq!(content_type, "application/json");
        let parsed = kobo_json::parse(&body).unwrap();
        assert_eq!(parsed.get("review_id").unwrap().as_str(), Some(id.as_str()));
        assert_eq!(parsed.get("ops").unwrap().as_array().unwrap().len(), 1);
        let op = &parsed.get("ops").unwrap().as_array().unwrap()[0];
        assert_eq!(op.get("text").unwrap().as_str(), Some("say \"hi\"\nthere"));
    }

    #[test]
    fn the_review_id_is_stable_and_sensitive_to_everything_that_matters() {
        let ops = vec![Op::Delete { block: "a".into() }];
        let id = review_id(GATEWAY, "s", "r", &ops);
        assert_eq!(id.len(), 64);
        assert_eq!(id, review_id(GATEWAY, "s", "r", &ops));
        assert_ne!(id, review_id(GATEWAY, "s", "r2", &ops));
        assert_ne!(id, review_id(GATEWAY, "s2", "r", &ops));
        assert_ne!(id, review_id("https://other.example", "s", "r", &ops));
        assert_ne!(
            id,
            review_id(GATEWAY, "s", "r", &[Op::Delete { block: "b".into() }])
        );
    }

    #[test]
    fn commit_bodies_list_decisions() {
        let Task::Post { body, .. } =
            commit(GATEWAY, &"a".repeat(64), &[("b1".into(), Take::Designer)])
        else {
            panic!("post")
        };
        let parsed = kobo_json::parse(&body).unwrap();
        let first = &parsed.get("decisions").unwrap().as_array().unwrap()[0];
        assert_eq!(first.get("take").unwrap().as_str(), Some("designer"));
    }

    #[test]
    fn replies_parse_and_malformed_ones_do_not() {
        let list = br#"{"draft":"v0.6","items":[{"id":"a1b2c3d4e5f6","file":"x/y.md","chapter":"C","title":"T","status":"Locked","blocks":3,"words":40}]}"#;
        let parsed = section_list(list).unwrap();
        assert_eq!(parsed.items[0].status, "Locked");
        assert!(section_list(b"not json").is_none());
        assert!(section_list(br#"{"items":[{"title":"no id"}]}"#).is_none());

        assert_eq!(
            review_reply(br#"{"status":"accepted","job_id":"j"}"#),
            Some(ReviewReply::Accepted("j".into()))
        );
        assert_eq!(
            review_reply(br#"{"status":"stale","message":"changed"}"#),
            Some(ReviewReply::Stale("changed".into()))
        );
        assert_eq!(review_reply(br#"{"status":"mystery"}"#), None);

        assert_eq!(
            commit_reply(br#"{"status":"committed","sha":"abc","file":"f"}"#),
            Some(CommitReply::Committed { sha: "abc".into() })
        );
        assert_eq!(
            commit_reply(br#"{"status":"unchanged"}"#),
            Some(CommitReply::Unchanged)
        );
    }

    #[test]
    fn a_finished_job_without_a_proposal_is_malformed() {
        assert!(job_reply(br#"{"state":"done","message":""}"#).is_none());
        let running = job_reply(br#"{"state":"running","message":"Revising"}"#).unwrap();
        assert_eq!(
            (running.state, running.message.as_str()),
            (JobState::Running, "Revising")
        );
        let done = br#"{"state":"done","message":"","proposal":{"title":"T","blocks":[
            {"id":"a","kind":"para","text":"New","spans":[],"change":"revised","why":"shorter",
             "before":{"id":"a","kind":"para","text":"Old","spans":[]}}],
            "flags":[{"block":"a","issue":"hmm"}],"dropped":0,
            "lint":{"available":true,"violations":0,"warnings":2,"new":[{"kind":"WARNING","message":"x"}]}}}"#;
        let job = job_reply(done).unwrap();
        let proposal = job.proposal.unwrap();
        assert_eq!(proposal.blocks[0].change, model::Change::Revised);
        assert_eq!(
            proposal.blocks[0].before.as_ref().unwrap().parts[0].text,
            "Old"
        );
        assert_eq!(proposal.lint.new, ["warning: x"]);
        assert_eq!(proposal.flags[0].issue, "hmm");
    }
}
