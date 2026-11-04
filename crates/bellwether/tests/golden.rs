//! Golden byte tests built from every example in the spec (rev 0.4,
//! section *Examples*, plus the introductory Terraform example).

use bellwether::{
    App, Event, Id, Kind, Message, Progress, RecordStore, Report, Scanner, State, Terminator, parse,
};

const ST: &[u8] = b"\x1b\\";

fn seq(body: &str) -> Vec<u8> {
    [b"\x1b]7501;", body.as_bytes(), ST].concat()
}

/// Checks that `body` parses to `expected` and that encoding `expected`
/// gives exactly the spec's bytes.
fn golden(body: &str, expected: &Report) {
    assert_eq!(&parse(body.as_bytes()).unwrap(), expected, "parse {body}");
    assert_eq!(
        expected.encode(Terminator::St),
        seq(body),
        "encode {}",
        String::from_utf8_lossy(&expected.encode(Terminator::St))
    );
}

fn app(s: &str) -> App {
    App::new(s).unwrap()
}
fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

#[test]
fn intro_terraform() {
    golden(
        "state=blocked:kind=permission:app=terraform:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/",
        &Report::builder(State::Blocked)
            .kind(Kind::Permission)
            .app(app("terraform"))
            .msg("Apply 3 to add, 1 to change, 0 to destroy?")
            .try_build()
            .unwrap(),
    );
}

fn brew_steps() -> Vec<(&'static str, Report)> {
    let working = Report::builder(State::Working)
        .app(app("brew"))
        .msg("Installing updates")
        .try_build()
        .unwrap();
    vec![
        (
            "state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz",
            working.clone(),
        ),
        (
            "state=blocked:kind=auth:app=brew:msg=UGFzc3dvcmQgcmVxdWlyZWQgdG8gaW5zdGFsbCB1cGRhdGVz",
            Report::builder(State::Blocked)
                .kind(Kind::Auth)
                .app(app("brew"))
                .msg("Password required to install updates")
                .try_build()
                .unwrap(),
        ),
        (
            "state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz",
            working,
        ),
        (
            "state=done:app=brew:msg=VXBncmFkZWQgMTIgcGFja2FnZXM=",
            Report::builder(State::Done)
                .app(app("brew"))
                .msg("Upgraded 12 packages")
                .try_build()
                .unwrap(),
        ),
    ]
}

#[test]
fn one_program_brew() {
    let mut store = RecordStore::new();
    for (body, expected) in brew_steps() {
        golden(body, &expected);
        store.apply(expected);
    }
    // "The done record survives the shell prompt that follows."
    store.on_prompt();
    let root = store.get(None).unwrap();
    assert_eq!(root.state(), State::Done);
    assert_eq!(root.msg(), Some("Upgraded 12 packages"));
}

#[test]
fn several_records_deploy() {
    let us_east = || {
        Report::builder(State::Working)
            .id(id("us-east"))
            .title("US East")
    };
    let steps: Vec<(&str, Report)> = vec![
        (
            "state=working:app=deploy:msg=RGVwbG95aW5nIHYyLjQuMQ==",
            Report::builder(State::Working)
                .app(app("deploy"))
                .msg("Deploying v2.4.1")
                .try_build()
                .unwrap(),
        ),
        (
            "state=working:id=us-east:title=VVMgRWFzdA==:progress=40:msg=UHVzaGluZyBpbWFnZQ==",
            us_east()
                .progress(Progress::new(40).unwrap())
                .msg("Pushing image")
                .try_build()
                .unwrap(),
        ),
        (
            "state=blocked:kind=permission:id=eu-west:title=RVUgV2VzdA==:msg=QXBwcm92ZSBkZXBsb3kgdG8gZXUtd2VzdCAocHJvZHVjdGlvbik/",
            Report::builder(State::Blocked)
                .kind(Kind::Permission)
                .id(id("eu-west"))
                .title("EU West")
                .msg("Approve deploy to eu-west (production)?")
                .try_build()
                .unwrap(),
        ),
        (
            "state=done:id=us-east:title=VVMgRWFzdA==:msg=SGVhbHRoeQ==",
            Report::builder(State::Done)
                .id(id("us-east"))
                .title("US East")
                .msg("Healthy")
                .try_build()
                .unwrap(),
        ),
        ("state=clear:id=us-east", Report::clear(Some(id("us-east")))),
        ("state=clear:id=eu-west", Report::clear(Some(id("eu-west")))),
        (
            "state=done:app=deploy:msg=RGVwbG95ZWQgdG8gMyByZWdpb25z",
            Report::builder(State::Done)
                .app(app("deploy"))
                .msg("Deployed to 3 regions")
                .try_build()
                .unwrap(),
        ),
    ];

    let mut store = RecordStore::new();
    for (i, (body, expected)) in steps.into_iter().enumerate() {
        golden(body, &expected);
        store.apply(expected);
        if i == 2 {
            // "The region records take app=deploy from the root when displayed.
            // While eu-west is blocked the root still says working."
            let eu = id("eu-west");
            assert_eq!(store.get(None).unwrap().state(), State::Working);
            assert_eq!(store.get(Some(&eu)).unwrap().state(), State::Blocked);
            assert_eq!(store.effective_app(Some(&eu)), Some(&app("deploy")));
            assert_eq!(
                store.effective_app(Some(&id("us-east"))),
                Some(&app("deploy"))
            );
            assert_eq!(store.len(), 3);
        }
    }
    assert_eq!(store.len(), 1);
    assert_eq!(store.get(None).unwrap().state(), State::Done);
}

#[test]
fn shell_function() {
    // status() {
    //   printf '\e]7501;state=%s:msg=%s\e\\' "$1" "$(printf '%s' "$2" | base64 | tr -d '\n')"
    // }
    let cases = [
        (
            State::Working,
            "Syncing photos",
            "state=working:msg=U3luY2luZyBwaG90b3M=",
        ),
        (
            State::Done,
            "Photos synced",
            "state=done:msg=UGhvdG9zIHN5bmNlZA==",
        ),
        (
            State::Error,
            "rsync failed",
            "state=error:msg=cnN5bmMgZmFpbGVk",
        ),
    ];
    for (state, msg, body) in cases {
        golden(body, &Report::builder(state).msg(msg).try_build().unwrap());
    }
}

#[test]
fn whole_examples_through_scanner() {
    // The brew example as one stream with output in between, terminated with
    // BEL, scanned one byte at a time.
    let mut stream = Vec::new();
    for (body, _) in brew_steps() {
        stream.extend_from_slice(b"==> output\r\n");
        stream.extend_from_slice(b"\x1b]7501;");
        stream.extend_from_slice(body.as_bytes());
        stream.push(0x07);
    }
    let mut scanner = Scanner::new();
    let mut found = Vec::new();
    for b in &stream {
        scanner.feed(std::slice::from_ref(b), |ev| {
            if let Event::Sequence {
                message: Message::Report(r),
                ..
            } = ev
            {
                found.push(r.unwrap());
            }
        });
    }
    let expected: Vec<Report> = brew_steps().into_iter().map(|(_, r)| r).collect();
    assert_eq!(found, expected);
}
