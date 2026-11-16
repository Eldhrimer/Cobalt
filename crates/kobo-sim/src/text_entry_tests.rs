//! The simulator drawing the runtime's text field and driving it by taps,
//! the way `kobo drive type` does.

use super::tests::private_temp_dir;
use super::{read_protocol_frame, write_protocol_frame, AppServer, AppSession};
use kobo_protocol::{DeviceRequest, DeviceResult, Frame, Message, SecretTarget, TextOutcome};
use kobo_ui::{ActionId, FieldKind, LayoutKind, NodeId, Screen, TopBar};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

fn frame(request_id: u32, message: Message) -> Frame {
    Frame {
        version: kobo_protocol::VERSION,
        request_id,
        message,
    }
}

fn ask(kind: FieldKind) -> DeviceRequest {
    DeviceRequest::TypeText {
        prompt: "New item".into(),
        hint: "Added \u{201c}milk\u{201d}.".into(),
        initial: String::new(),
        submit: "Add".into(),
        kind,
        save_as: (kind == FieldKind::Secret).then(|| SecretTarget::named("lichess")),
    }
}

/// An application that draws one titled screen, asks for a field, and
/// reports the answer it gets.
fn application(
    socket: &Path,
    name: &'static str,
    kind: FieldKind,
) -> thread::JoinHandle<(u32, DeviceResult)> {
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        let mut stream = UnixStream::connect(socket).expect("connect");
        write_protocol_frame(
            &mut stream,
            &frame(
                1,
                Message::Hello {
                    name: name.to_owned(),
                },
            ),
        )
        .expect("hello");
        read_protocol_frame(&mut stream).expect("welcome");
        write_protocol_frame(&mut stream, &frame(2, Message::DeviceRequest(ask(kind))))
            .expect("ask");
        let mut screen = Screen::new(1, Vec::new());
        screen.top_bar = Some(TopBar::new(NodeId(1), "Todo: 1 left"));
        write_protocol_frame(&mut stream, &frame(3, Message::SetScreen(screen))).expect("screen");
        loop {
            let answer = read_protocol_frame(&mut stream).expect("an answer");
            if let Message::DeviceResult(result) = answer.message {
                return (answer.request_id, result);
            }
        }
    })
}

fn wait_for_keyboard(session: &AppSession) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let screen = session.screen();
        if screen
            .top_bar
            .as_ref()
            .is_some_and(|bar| bar.title == "Todo: 1 left")
            && format!("{screen:?}").contains("New item")
        {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("the keyboard never came up: {:?}", session.screen());
}

/// Taps the middle of whatever sends `name`, at panel coordinates, through
/// the same input path a browser or `kobo drive` uses.
fn tap(session: &AppSession, name: &str) {
    let action = if name == "back" {
        ActionId::BACK
    } else {
        ActionId::from_name(name)
    };
    let (x, y) = {
        let state = session.state.lock().expect("state");
        let metrics = super::profile_metrics().oriented(state.orientation);
        let layout = state.screen.layout_with(&metrics, &state.chrome);
        let rect = layout
            .nodes
            .iter()
            .find(|node| match node.kind {
                LayoutKind::Cell(hit, ..) | LayoutKind::BarAction(hit) => hit == action,
                LayoutKind::Back => action == ActionId::BACK,
                _ => false,
            })
            .unwrap_or_else(|| panic!("{name} is not on the panel"))
            .rect;
        (rect.x + rect.width / 2, rect.y + rect.height / 2)
    };
    session
        .replay_input(&format!("tap x={x}&y={y}"))
        .expect("the tap is taken");
}

#[test]
fn the_simulator_draws_the_runtime_field_and_types_into_it_by_tapping_keys() {
    let root = private_temp_dir();
    let socket = root.join("app.sock");
    let server = AppServer::bind("127.0.0.1:0", &socket).expect("bind");
    let app = application(&socket, "todo", FieldKind::Text);
    let session = server.accept_app().expect("accept");
    wait_for_keyboard(&session);
    let drawn = format!("{:?}", session.screen());
    assert!(drawn.contains("Added \u{201c}milk\u{201d}."), "{drawn}");
    for key in [
        "kb.shift", "kb.r0c0", "kb.r0c1", "kb.space", "kb.layer", "kb.r0c4",
    ] {
        tap(&session, key);
    }
    assert!(
        format!("{:?}", session.screen()).contains("Qw 5"),
        "the field shows what was typed without asking the application"
    );
    tap(&session, "kb.enter");
    let (request_id, answer) = app.join().expect("application");
    assert_eq!(request_id, 2, "the answer names the request it answers");
    assert_eq!(
        answer,
        DeviceResult::Text(TextOutcome::Typed("Qw 5".into()))
    );
    assert!(
        !format!("{:?}", session.screen()).contains("kb.r0c0"),
        "the application's own screen is back"
    );
    drop(session);
    drop(server);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_secret_typed_in_the_simulator_is_kept_for_the_app_and_never_sent_to_it() {
    let root = private_temp_dir();
    let secrets = private_temp_dir();
    let socket = root.join("app.sock");
    let server = AppServer::bind("127.0.0.1:0", &socket).expect("bind");
    let app = application(&socket, "lichess", FieldKind::Secret);
    let session = server.accept_app().expect("accept");
    session
        .state
        .lock()
        .expect("state")
        .secret_directory
        .clone_from(&secrets);
    wait_for_keyboard(&session);
    for key in ["kb.r0c0", "kb.r0c1", "kb.layer", "kb.r1c9"] {
        tap(&session, key);
    }
    let hidden = format!("{:?}", session.screen());
    assert!(!hidden.contains("qw_"), "{hidden}");
    assert!(hidden.contains("3 characters entered"), "{hidden}");
    tap(&session, "kb.reveal");
    assert!(format!("{:?}", session.screen()).contains("qw_"));
    tap(&session, "kb.enter");
    let (_, answer) = app.join().expect("application");
    assert_eq!(answer, DeviceResult::Text(TextOutcome::Saved));
    assert_eq!(
        std::fs::read(secrets.join("apps/lichess/lichess")).expect("the secret was kept"),
        b"qw_"
    );
    let log = session.state.lock().expect("state").logs.join("\n");
    assert!(!log.contains("qw_"), "{log}");
    drop(session);
    drop(server);
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(secrets).ok();
}

#[test]
fn back_in_the_simulator_cancels_the_field() {
    let root = private_temp_dir();
    let socket = root.join("app.sock");
    let server = AppServer::bind("127.0.0.1:0", &socket).expect("bind");
    let app = application(&socket, "todo", FieldKind::Address);
    let session = server.accept_app().expect("accept");
    wait_for_keyboard(&session);
    tap(&session, "kb.at");
    tap(&session, "back");
    let (_, answer) = app.join().expect("application");
    assert_eq!(answer, DeviceResult::Text(TextOutcome::Cancelled));
    drop(session);
    drop(server);
    std::fs::remove_dir_all(root).ok();
}

/// The entry as the simulator draws it, caret and all, and where its first
/// line is on the panel: its left edge and its middle.
fn entry(session: &AppSession) -> (String, u32, u32, u32) {
    let state = session.state.lock().expect("state");
    let metrics = super::profile_metrics().oriented(state.orientation);
    let layout = state.screen.layout_with(&metrics, &state.chrome);
    let bounds = layout
        .nodes
        .iter()
        .find(|node| matches!(node.kind, LayoutKind::RichText(_)) && node.text_lines.is_empty())
        .expect("the entry is laid out")
        .rect;
    let text = state
        .screen
        .nodes
        .iter()
        .find_map(|node| match node {
            kobo_ui::Node::RichText { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("the entry is drawn");
    let lines = i32::try_from(text.lines().count()).expect("a few lines");
    let at = |value: i32| u32::try_from(value).expect("on the panel");
    (
        text,
        at(bounds.x + 1),
        at(bounds.y + bounds.height / lines / 2),
        at(bounds.x + bounds.width - 1),
    )
}

#[test]
fn a_route_places_the_caret_drags_a_selection_and_double_taps_a_word_in_a_field() {
    let root = private_temp_dir();
    let socket = root.join("app.sock");
    let server = AppServer::bind("127.0.0.1:0", &socket).expect("bind");
    let app = application(&socket, "todo", FieldKind::Prose);
    let session = server.accept_app().expect("accept");
    wait_for_keyboard(&session);
    for key in ["kb.r0c0", "kb.r0c1", "kb.space", "kb.r0c2"] {
        tap(&session, key);
    }
    let (text, left, middle, right) = entry(&session);
    assert_eq!(text, "qw e|");
    // tap-at: the caret goes to the start of the line.
    session
        .replay_input(&format!("tap x={left}&y={middle}"))
        .expect("a tap in the text");
    tap(&session, "kb.r0c3");
    assert_eq!(entry(&session).0, "r|qw e");
    // drag-at: across the whole line, and the next key replaces it.
    session
        .replay_input(&format!(
            "drag x1={left}&y1={middle}&x2={right}&y2={middle}"
        ))
        .expect("a drag in the text");
    assert_eq!(entry(&session).0, "rqw e", "selected, so no caret");
    tap(&session, "kb.r0c4");
    tap(&session, "kb.space");
    tap(&session, "kb.r0c5");
    assert_eq!(entry(&session).0, "t y|");
    // Two taps in two steps are two taps; double-tap-at takes the word.
    session
        .replay_input(&format!("tap x={left}&y={middle}"))
        .expect("a tap");
    session
        .replay_input(&format!("tap x={left}&y={middle}"))
        .expect("a second tap");
    assert_eq!(entry(&session).0, "|t y");
    session
        .replay_input(&format!("double-tap x={left}&y={middle}"))
        .expect("a double tap");
    tap(&session, "kb.r0c6");
    assert_eq!(entry(&session).0, "u| y");
    tap(&session, "kb.enter");
    let (_, answer) = app.join().expect("application");
    assert_eq!(answer, DeviceResult::Text(TextOutcome::Typed("u y".into())));
    drop(session);
    drop(server);
    std::fs::remove_dir_all(root).ok();
}
