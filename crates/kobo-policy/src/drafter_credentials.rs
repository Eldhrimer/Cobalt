//! Reviewed Drafter gateway routes for an account bound to its owner's server.
//! The contract is documented in apps/drafter/README.md.
//!
//! The token is only ever attached to five routes under the saved server, and
//! the two writes carry exactly the fields the gateway documents. A compromised
//! app cannot turn the token into anything else: no other path, no other
//! method, no extra fields.
use kobo_json::Value;
use kobo_protocol::{Credential, CredentialUse, SecretHeader};

const MAX_BODY: usize = 48 * 1024;
const MAX_OPS: usize = 200;
const MAX_TEXT: usize = 8 * 1024;
const MAX_ID: usize = 64;
const ASKS: [&str; 7] = [
    "tighten", "plainer", "concrete", "cut", "keep", "check", "custom",
];

pub(super) fn allowed(
    credential: &Credential,
    server: &str,
    url: &str,
    usage: CredentialUse,
    body: Option<&str>,
    content_type: Option<&str>,
) -> bool {
    if url.contains('#')
        || url.chars().any(char::is_control)
        || credential.secret != "wu-drafter"
        || credential.header != SecretHeader::Bearer
        || !super::servers::contains(server, url)
    {
        return false;
    }
    let (Ok(base), Ok(target)) = (kobo_net::parse(server), kobo_net::parse(url)) else {
        return false;
    };
    let Some(route) = target.path.strip_prefix(base.path.trim_end_matches('/')) else {
        return false;
    };
    if route.contains(['%', '\\', '#', '?']) {
        return false;
    }
    match usage {
        CredentialUse::Fetch => body.is_none() && content_type.is_none() && read_route(route),
        CredentialUse::Post => {
            content_type == Some("application/json")
                && body.is_some_and(|body| match route {
                    "/reviews" => review_body(body),
                    "/commits" => commit_body(body),
                    _ => false,
                })
        }
        _ => false,
    }
}

fn hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_route(route: &str) -> bool {
    if route == "/sections" {
        return true;
    }
    if let Some(id) = route.strip_prefix("/sections/") {
        return hex(id, 12);
    }
    route.strip_prefix("/jobs/").is_some_and(|id| hex(id, 32))
}

fn plain(text: &str) -> bool {
    !text.chars().any(char::is_control) && !text.contains(['\\', '"'])
}

fn id_ok(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| !id.is_empty() && id.len() <= MAX_ID && plain(id))
}

fn optional_id(value: &Value) -> bool {
    matches!(value, Value::Null) || id_ok(value)
}

fn words(value: &Value, allow_empty: bool) -> bool {
    value
        .as_str()
        .is_some_and(|text| (allow_empty || !text.trim().is_empty()) && text.len() <= MAX_TEXT)
}

fn count(value: &Value) -> bool {
    value.as_i64().is_some_and(|n| (0..=1_000_000).contains(&n))
}

/// The fields of an object, exactly these names and no others, in any order.
fn fields<'a>(value: &'a Value, names: &[&str]) -> Option<Vec<&'a Value>> {
    let Value::Object(entries) = value else {
        return None;
    };
    if entries.len() != names.len() {
        return None;
    }
    let mut found = Vec::with_capacity(names.len());
    for name in names {
        let mut matches = entries.iter().filter(|(key, _)| key == name);
        let (_, value) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        found.push(value);
    }
    Some(found)
}

fn op_ok(op: &Value) -> bool {
    let Some(kind) = op.get("op").and_then(Value::as_str) else {
        return false;
    };
    let check = |names: &[&str], rules: &[fn(&Value) -> bool]| {
        let mut all = vec!["op"];
        all.extend_from_slice(names);
        fields(op, &all).is_some_and(|values| {
            values[1..]
                .iter()
                .zip(rules)
                .all(|(value, rule)| rule(value))
        })
    };
    let ask: fn(&Value) -> bool = |v| v.as_str().is_some_and(|a| ASKS.contains(&a));
    match kind {
        "mark" => check(
            &["block", "part", "start", "end", "text", "ask", "note"],
            &[
                id_ok,
                count,
                count,
                count,
                |v| words(v, false),
                ask,
                |v| words(v, true),
            ],
        ),
        "note" => check(&["block", "note"], &[id_ok, |v| words(v, false)]),
        "want" => check(&["after", "note"], &[optional_id, |v| words(v, false)]),
        "move" => check(&["block", "after"], &[id_ok, optional_id]),
        "split" => check(&["block", "at", "new"], &[id_ok, count, id_ok]),
        "merge" | "delete" => check(&["block"], &[id_ok]),
        "edit" => check(&["block", "text"], &[id_ok, |v| words(v, false)]),
        "insert" => check(
            &["after", "text", "new"],
            &[optional_id, |v| words(v, false), id_ok],
        ),
        _ => false,
    }
}

fn review_body(body: &str) -> bool {
    if body.len() > MAX_BODY {
        return false;
    }
    let Ok(root) = kobo_json::parse(body) else {
        return false;
    };
    let Some(values) = fields(&root, &["review_id", "section_id", "rev", "ops"]) else {
        return false;
    };
    let [review_id, section_id, rev, ops] = values[..] else {
        return false;
    };
    review_id.as_str().is_some_and(|id| hex(id, 64))
        && section_id.as_str().is_some_and(|id| hex(id, 12))
        && rev.as_str().is_some_and(|r| {
            !r.is_empty()
                && r.len() <= MAX_ID
                && r.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        && ops
            .as_array()
            .is_some_and(|list| list.len() <= MAX_OPS && list.iter().all(op_ok))
}

fn commit_body(body: &str) -> bool {
    if body.len() > MAX_BODY {
        return false;
    }
    let Ok(root) = kobo_json::parse(body) else {
        return false;
    };
    let Some(values) = fields(&root, &["review_id", "decisions"]) else {
        return false;
    };
    let [review_id, decisions] = values[..] else {
        return false;
    };
    review_id.as_str().is_some_and(|id| hex(id, 64))
        && decisions.as_array().is_some_and(|list| {
            list.len() <= MAX_OPS
                && list.iter().all(|d| {
                    fields(d, &["block", "take"]).is_some_and(|v| {
                        id_ok(v[0])
                            && v[1]
                                .as_str()
                                .is_some_and(|t| t == "proposal" || t == "designer")
                    })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::allowed_request_with_server;

    const SERVER: &str = "https://drafter.example:8443/wu";
    const REVIEW: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    fn request(route: &str, usage: CredentialUse, body: Option<&str>) -> bool {
        allowed_request_with_server(
            "drafter",
            &Credential::bearer("wu-drafter"),
            &format!("{SERVER}{route}"),
            usage,
            body,
            body.map(|_| "application/json"),
            Some(SERVER),
        )
    }

    fn review(ops: &str) -> String {
        format!(
            r#"{{"review_id":"{REVIEW}","section_id":"a1b2c3d4e5f6","rev":"3f2a9c0d11aa","ops":{ops}}}"#
        )
    }

    #[test]
    fn the_token_reads_exactly_the_documented_routes() {
        let job = "0123456789abcdef0123456789abcdef";
        for route in [
            "/sections",
            "/sections/a1b2c3d4e5f6",
            &format!("/jobs/{job}"),
        ] {
            assert!(request(route, CredentialUse::Fetch, None), "{route}");
        }
        for route in [
            "/sections?x=1",
            "/sections/",
            "/sections/A1B2C3D4E5F6",
            "/sections/a1b2c3d4e5f",
            "/sections/a1b2c3d4e5f6/extra",
            "/sections/%61%31b2c3d4e5f6",
            "/sections/..%2f..",
            "/jobs/abc",
            "/jobs/0123456789ABCDEF0123456789ABCDEF",
            "/reviews",
            "/commits",
            "/",
            "/admin",
        ] {
            assert!(!request(route, CredentialUse::Fetch, None), "{route}");
        }
        assert!(!request("/sections", CredentialUse::Fetch, Some("{}")));
    }

    #[test]
    fn a_review_carries_exactly_the_documented_fields_and_operations() {
        let ops = r#"[
          {"op":"mark","block":"p1","part":0,"start":3,"end":9,"text":"a phrase","ask":"tighten","note":""},
          {"op":"note","block":"p1","note":"this drags"},
          {"op":"want","after":null,"note":"an example"},
          {"op":"move","block":"p1","after":"p2"},
          {"op":"split","block":"p1","at":12,"new":"d1"},
          {"op":"merge","block":"d1"},
          {"op":"delete","block":"p3"},
          {"op":"edit","block":"p2","text":"retyped, with \"quotes\" and \n newlines"},
          {"op":"insert","after":"p1","text":"A paragraph.","new":"d2"}]"#;
        assert!(request("/reviews", CredentialUse::Post, Some(&review(ops))));
        assert!(request(
            "/reviews",
            CredentialUse::Post,
            Some(&review("[]"))
        ));

        let bad = [
            review(r#"[{"op":"explode","block":"p1"}]"#),
            review(r#"[{"op":"delete","block":"p1","extra":1}]"#),
            review(r#"[{"op":"delete"}]"#),
            review(
                r#"[{"op":"mark","block":"p1","part":0,"start":0,"end":1,"text":"x","ask":"obliterate","note":""}]"#,
            ),
            review(r#"[{"op":"split","block":"p1","at":-1,"new":"d1"}]"#),
            review(r#"[{"op":"move","block":"p\"1","after":null}]"#),
            review(r#"[{"op":"edit","block":"p1","text":"  "}]"#),
            review(r#"[{"op":"note","block":"p1","note":123}]"#),
            review(r#"[{"op":"delete","block":"p1","block":"p2"}]"#),
            format!(
                r#"{{"review_id":"{REVIEW}","section_id":"a1b2c3d4e5f6","rev":"r","ops":[],"extra":1}}"#
            ),
            format!(r#"{{"review_id":"{REVIEW}","section_id":"a1b2c3d4e5f6","ops":[]}}"#),
            r#"{"review_id":"short","section_id":"a1b2c3d4e5f6","rev":"r","ops":[]}"#.to_string(),
            format!(r#"{{"review_id":"{REVIEW}","section_id":"NOTHEX123456","rev":"r","ops":[]}}"#),
            format!(
                r#"{{"review_id":"{REVIEW}","section_id":"a1b2c3d4e5f6","rev":"bad rev","ops":[]}}"#
            ),
            "{}".to_owned(),
            "not json".to_owned(),
        ];
        for body in &bad {
            assert!(
                !request("/reviews", CredentialUse::Post, Some(body)),
                "{body}"
            );
        }
        let many = format!(
            "[{}]",
            vec![r#"{"op":"merge","block":"p1"}"#; MAX_OPS + 1].join(",")
        );
        assert!(!request(
            "/reviews",
            CredentialUse::Post,
            Some(&review(&many))
        ));
        let huge = review(&format!(
            r#"[{{"op":"edit","block":"p1","text":"{}"}}]"#,
            "x".repeat(MAX_TEXT + 1)
        ));
        assert!(!request("/reviews", CredentialUse::Post, Some(&huge)));
        assert!(!request(
            "/reviews",
            CredentialUse::Post,
            Some(&" ".repeat(MAX_BODY + 1))
        ));
    }

    #[test]
    fn a_commit_names_a_review_and_per_block_decisions_only() {
        let ok = format!(
            r#"{{"review_id":"{REVIEW}","decisions":[{{"block":"p1","take":"proposal"}},{{"block":"ai1","take":"designer"}}]}}"#
        );
        assert!(request("/commits", CredentialUse::Post, Some(&ok)));
        assert!(request(
            "/commits",
            CredentialUse::Post,
            Some(&format!(r#"{{"review_id":"{REVIEW}","decisions":[]}}"#))
        ));
        for body in [
            format!(r#"{{"review_id":"{REVIEW}","decisions":[{{"block":"p1","take":"both"}}]}}"#),
            format!(r#"{{"review_id":"{REVIEW}","decisions":[{{"block":"p1"}}]}}"#),
            format!(
                r#"{{"review_id":"{REVIEW}","decisions":[{{"block":"p1","take":"proposal","x":1}}]}}"#
            ),
            format!(r#"{{"review_id":"{REVIEW}","decisions":[],"branch":"main"}}"#),
            format!(r#"{{"review_id":"{REVIEW}"}}"#),
            r#"{"decisions":[]}"#.to_owned(),
        ] {
            assert!(
                !request("/commits", CredentialUse::Post, Some(&body)),
                "{body}"
            );
        }
        assert!(!request("/commits", CredentialUse::Fetch, None));
        assert!(!request("/sections", CredentialUse::Post, Some("{}")));
    }

    #[test]
    fn writes_other_than_post_are_never_allowed() {
        for usage in [CredentialUse::Put, CredentialUse::Patch] {
            assert!(!request("/reviews", usage, Some(&review("[]"))));
        }
    }

    #[test]
    fn the_token_never_escapes_its_saved_server_or_convention() {
        let token = Credential::bearer("wu-drafter");
        for url in [
            "https://other.example:8443/wu/sections",
            "http://drafter.example:8443/wu/sections",
            "https://drafter.example:8443/sections",
            "https://drafter.example:8443/wu-other/sections",
            "https://drafter.example:8443/wu/sections#x",
            "https://drafter.example/wu/sections",
        ] {
            assert!(
                !allowed(&token, SERVER, url, CredentialUse::Fetch, None, None),
                "{url}"
            );
        }
        for credential in [
            Credential::basic("wu-drafter"),
            Credential::in_header("wu-drafter", "X-Auth-Token"),
            Credential::bearer("hermes-post"),
        ] {
            assert!(!allowed(
                &credential,
                SERVER,
                &format!("{SERVER}/sections"),
                CredentialUse::Fetch,
                None,
                None
            ));
        }
        assert!(!allowed_request_with_server(
            "post",
            &token,
            &format!("{SERVER}/sections"),
            CredentialUse::Fetch,
            None,
            None,
            Some(SERVER)
        ));
    }
}
