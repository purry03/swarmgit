//! Merge requests. Each action on one (opening it, a comment, a review, new
//! commits, closing) is a git commit signed by whoever did it, so it can never
//! be edited, and `git log` reads it. The commits live in
//! `refs/mr/<merge request>/<author key>`: one append-only chain per author.
//!
//! A commit holds the text as its message, followed by `Swarm-*` trailers.
//! Its tree is empty. Its parents are the author's previous event and, when
//! the event proposes code, the proposed commit, which keeps that code
//! reachable. The signature is in a `swarmsig` header and covers every other
//! byte of the commit.

use std::collections::BTreeMap;

use data_encoding::BASE64;

use crate::about::is_valid_text;
use crate::consts::{
    CTX_SIG_MR, MAX_MR_EVENT_BYTES, MAX_MR_NAME_BYTES, MAX_REFNAME_BYTES, SUITE1_BUNDLE_LEN,
    SUITE1_SIG_LEN, SUITE_HYBRID,
};
use crate::crypto::key_id;
use crate::error::ErrorCode;
use crate::ids::{KeyId, RepoId};
use crate::pq::{self, Signer};
use crate::refname::is_valid_refname;

/// Where merge request refs live.
pub const MR_REF_PREFIX: &str = "refs/mr/";

const SIG_HEADER: &str = "swarmsig ";
const SIG_LINE: usize = 76;
const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const EMPTY_TREE_SHA256: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";

/// What an event does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Opens a merge request. Its object ID names the merge request.
    Open,
    /// Proposes a new head. Counts only from the merge request's author.
    Update,
    /// A comment, on the whole request or on a line of a file.
    Comment,
    /// A review in favour of the head named by `commit`.
    Approve,
    /// A review asking for changes to the head named by `commit`.
    RequestChanges,
    /// Closes the request.
    Close,
    /// Opens a closed request again.
    Reopen,
}

const KINDS: [(Kind, &str); 7] = [
    (Kind::Open, "open"),
    (Kind::Update, "update"),
    (Kind::Comment, "comment"),
    (Kind::Approve, "approve"),
    (Kind::RequestChanges, "request-changes"),
    (Kind::Close, "close"),
    (Kind::Reopen, "reopen"),
];

impl Kind {
    /// The name used in the trailer.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        KINDS
            .iter()
            .find(|(kind, _)| *kind == self)
            .map_or("comment", |(_, name)| name)
    }

    /// Parse a trailer name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        KINDS
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(kind, _)| *kind)
    }

    /// True for the kinds that carry code.
    #[must_use]
    pub fn proposes_code(self) -> bool {
        matches!(self, Self::Open | Self::Update)
    }

    fn is_review(self) -> bool {
        matches!(self, Self::Approve | Self::RequestChanges)
    }
}

/// One event, before it is signed. Object IDs are lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Repo the event belongs to.
    pub repo: RepoId,
    /// What it does.
    pub kind: Kind,
    /// The merge request, absent on `open`.
    pub mr: Option<String>,
    /// The author's previous event in this merge request.
    pub prev: Option<String>,
    /// Proposed commit (`open` and `update`).
    pub head: Option<String>,
    /// Where `head` left the target branch (`open` and `update`).
    pub base: Option<String>,
    /// Branch to merge into (`open`).
    pub target: Option<String>,
    /// The author's name for the work, normally their branch (`open`).
    pub topic: Option<String>,
    /// Event being answered (`comment`).
    pub reply_to: Option<String>,
    /// File a comment is about.
    pub file: Option<String>,
    /// Line in `file`, counted from 1.
    pub line: Option<u32>,
    /// Head the author was looking at (reviews and line comments).
    pub commit: Option<String>,
    /// Display name the author gives. Not proof of who they are.
    pub name: String,
    /// Unix seconds.
    pub ts: u64,
    /// The words. On `open` the first line is the title.
    pub text: String,
}

/// An event whose signature and shape have been checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEvent {
    /// Object ID of the commit.
    pub id: String,
    /// Key that signed it.
    pub signer: KeyId,
    /// The fields.
    pub event: Event,
}

/// True for a lowercase hex object ID of either format.
#[must_use]
pub fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The ref holding `author`'s events on `mr`.
#[must_use]
pub fn mr_ref(mr: &str, author: KeyId) -> String {
    format!("{MR_REF_PREFIX}{mr}/{}", hex(author.as_bytes()))
}

/// Split a ref name made by [`mr_ref`].
#[must_use]
pub fn parse_mr_ref(name: &str) -> Option<(String, KeyId)> {
    let (mr, author) = name.strip_prefix(MR_REF_PREFIX)?.split_once('/')?;
    if !is_oid(mr) || author.len() != 64 || !is_oid(author) {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&author[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some((mr.to_string(), KeyId::from_bytes(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A display name that fits a git author line.
#[must_use]
pub fn clean_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if matches!(c, '<' | '>') || !is_valid_text(c.encode_utf8(&mut [0; 4]), 4) {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_MR_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    let out = out.trim();
    if out.is_empty() {
        "anonymous".to_string()
    } else {
        out.to_string()
    }
}

/// Make written text fit an event: no carriage returns, terminal escapes or
/// bidirectional overrides, no blank lines at either end.
#[must_use]
pub fn clean_text(text: &str) -> String {
    let cleaned: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| {
            line.chars()
                .filter(|&c| c == '\t' || is_valid_text(c.encode_utf8(&mut [0; 4]), 4))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    cleaned.join("\n").trim_matches('\n').to_string()
}

impl Event {
    /// An event with only the required fields set.
    #[must_use]
    pub fn new(repo: RepoId, kind: Kind, name: &str, ts: u64) -> Self {
        Self {
            repo,
            kind,
            mr: None,
            prev: None,
            head: None,
            base: None,
            target: None,
            topic: None,
            reply_to: None,
            file: None,
            line: None,
            commit: None,
            name: clean_name(name),
            ts,
            text: String::new(),
        }
    }

    /// First line of the text.
    #[must_use]
    pub fn title(&self) -> &str {
        self.text.lines().next().unwrap_or_default()
    }

    /// The text after the first line.
    #[must_use]
    pub fn body(&self) -> &str {
        self.text
            .split_once('\n')
            .map_or("", |(_, rest)| rest.trim_matches('\n'))
    }

    fn check(&self) -> Result<(), ErrorCode> {
        let oid_ok = |value: &Option<String>| value.as_deref().is_none_or(is_oid);
        let ids = [
            &self.mr,
            &self.prev,
            &self.head,
            &self.base,
            &self.reply_to,
            &self.commit,
        ];
        let open = self.kind == Kind::Open;
        let line_ok = |value: &Option<String>| {
            value
                .as_deref()
                .is_none_or(|v| !v.is_empty() && is_valid_text(v, 1024))
        };
        let shape = ids.into_iter().all(oid_ok)
            && open == self.mr.is_none()
            && open == self.target.is_some()
            && (!open || (self.prev.is_none() && !self.title().is_empty()))
            && self.kind.proposes_code() == self.head.is_some()
            && (self.base.is_none() || self.kind.proposes_code())
            && (self.topic.is_none() || open)
            && (!self.kind.is_review() || self.commit.is_some())
            && (self.kind != Kind::Comment || !self.text.is_empty())
            && (self.kind == Kind::Comment
                || (self.file.is_none() && self.line.is_none() && self.reply_to.is_none()))
            && (self.line.is_none() || (self.file.is_some() && self.line != Some(0)))
            && line_ok(&self.file)
            && line_ok(&self.topic)
            && self.target.as_deref().is_none_or(|t| {
                t.len() <= MAX_REFNAME_BYTES && t.starts_with("refs/heads/") && is_valid_refname(t)
            })
            && self.name == clean_name(&self.name)
            && self.text == clean_text(&self.text);
        shape.then_some(()).ok_or(ErrorCode::Malformed)
    }

    fn parents(&self) -> impl Iterator<Item = &String> {
        self.prev.iter().chain(self.head.iter())
    }

    /// The commit without its signature header, split where that header goes.
    fn payload(&self, signer: KeyId) -> (String, String) {
        let sha256 = self
            .mr
            .as_deref()
            .or(self.head.as_deref())
            .or(self.commit.as_deref())
            .is_some_and(|oid| oid.len() == 64);
        let tree = if sha256 {
            EMPTY_TREE_SHA256
        } else {
            EMPTY_TREE_SHA1
        };
        let mut headers = format!("tree {tree}\n");
        for parent in self.parents() {
            headers.push_str(&format!("parent {parent}\n"));
        }
        let who = format!(
            "{} <{}@swarm> {} +0000",
            self.name,
            &hex(signer.as_bytes())[..16],
            self.ts
        );
        headers.push_str(&format!("author {who}\ncommitter {who}\n"));

        let mut message = if self.text.is_empty() {
            format!("({})", self.kind.as_str())
        } else {
            self.text.clone()
        };
        message.push_str("\n\n");
        let line = self.line.map(|line| line.to_string());
        let repo = self.repo.to_base32();
        for (key, value) in [
            ("Kind", Some(self.kind.as_str())),
            ("Repo", Some(repo.as_str())),
            ("MR", self.mr.as_deref()),
            ("Prev", self.prev.as_deref()),
            ("Head", self.head.as_deref()),
            ("Base", self.base.as_deref()),
            ("Target", self.target.as_deref()),
            ("Topic", self.topic.as_deref()),
            ("Reply-To", self.reply_to.as_deref()),
            ("File", self.file.as_deref()),
            ("Line", line.as_deref()),
            ("Commit", self.commit.as_deref()),
        ] {
            if let Some(value) = value {
                message.push_str(&format!("Swarm-{key}: {value}\n"));
            }
        }
        (headers, format!("\n{message}"))
    }

    /// Sign the event, returning the bytes of its commit object.
    pub fn sign(&self, signer: &dyn Signer) -> Result<Vec<u8>, ErrorCode> {
        self.check()?;
        let bundle = signer.bundle();
        let (headers, message) = self.payload(key_id(&bundle));
        let signature = signer
            .try_sign(&sig_input(&headers, &message))
            .map_err(|_| ErrorCode::BadSignature)?;
        let encoded = BASE64.encode(&[bundle, signature].concat());
        let mut commit = headers;
        commit.push_str(SIG_HEADER);
        for (index, chunk) in encoded.as_bytes().chunks(SIG_LINE).enumerate() {
            if index > 0 {
                commit.push(' ');
            }
            commit.push_str(&String::from_utf8_lossy(chunk));
            commit.push('\n');
        }
        commit.push_str(&message);
        if commit.len() > MAX_MR_EVENT_BYTES {
            return Err(ErrorCode::TooLarge);
        }
        Ok(commit.into_bytes())
    }
}

fn sig_input(headers: &str, message: &str) -> Vec<u8> {
    [CTX_SIG_MR, headers.as_bytes(), message.as_bytes()].concat()
}

/// Check the commit object `bytes`, whose ID is `id`.
pub fn verify_event(id: &str, bytes: &[u8]) -> Result<VerifiedEvent, ErrorCode> {
    if bytes.len() > MAX_MR_EVENT_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ErrorCode::Malformed)?;
    // At the start of a line: a display name may hold the same word.
    let at = text
        .find(&format!("\n{SIG_HEADER}"))
        .ok_or(ErrorCode::Malformed)?
        + 1;
    let (headers, rest) = (&text[..at], &text[at + SIG_HEADER.len()..]);
    let mut encoded = String::new();
    let mut at = 0;
    for (index, line) in rest.split_inclusive('\n').enumerate() {
        let part = if index == 0 {
            line
        } else {
            match line.strip_prefix(' ') {
                Some(part) => part,
                None => break,
            }
        };
        encoded.push_str(part.trim_end_matches('\n'));
        at += line.len();
    }
    let message = &rest[at..];
    let signed = BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| ErrorCode::BadSignature)?;
    if signed.len() != SUITE1_BUNDLE_LEN + SUITE1_SIG_LEN {
        return Err(ErrorCode::BadSignature);
    }
    let (bundle, signature) = signed.split_at(SUITE1_BUNDLE_LEN);
    pq::verify(
        SUITE_HYBRID,
        bundle,
        &sig_input(headers, message),
        signature,
    )
    .map_err(|_| ErrorCode::BadSignature)?;
    let signer = key_id(bundle);

    let event = parse(headers, message)?;
    event.check()?;
    // Only the one way of writing an event is accepted, so what git shows and
    // what is read here cannot differ.
    let (want_headers, want_message) = event.payload(signer);
    if want_headers != headers || want_message != message {
        return Err(ErrorCode::NonCanonical);
    }
    Ok(VerifiedEvent {
        id: id.to_string(),
        signer,
        event,
    })
}

/// Read the fields out of a commit. The caller re-encodes them to reject
/// anything this reads loosely.
fn parse(headers: &str, message: &str) -> Result<Event, ErrorCode> {
    let author = headers
        .lines()
        .find_map(|line| line.strip_prefix("author "))
        .ok_or(ErrorCode::Malformed)?;
    let (name, rest) = author.split_once(" <").ok_or(ErrorCode::Malformed)?;
    let ts = rest
        .split_once("> ")
        .and_then(|(_, when)| when.split(' ').next())
        .and_then(|secs| secs.parse().ok())
        .ok_or(ErrorCode::Malformed)?;

    let message = message.strip_prefix('\n').ok_or(ErrorCode::Malformed)?;
    let (text, trailers) = message.rsplit_once("\n\n").ok_or(ErrorCode::Malformed)?;
    let mut fields = BTreeMap::new();
    for line in trailers.lines() {
        let (key, value) = line
            .strip_prefix("Swarm-")
            .and_then(|line| line.split_once(": "))
            .ok_or(ErrorCode::Malformed)?;
        fields.insert(key, value.to_string());
    }
    let kind = fields
        .get("Kind")
        .and_then(|kind| Kind::parse(kind))
        .ok_or(ErrorCode::Malformed)?;
    let repo = fields
        .get("Repo")
        .and_then(|repo| RepoId::from_base32(repo).ok())
        .ok_or(ErrorCode::Malformed)?;
    let line = match fields.get("Line") {
        Some(line) => Some(line.parse().map_err(|_| ErrorCode::Malformed)?),
        None => None,
    };
    let mut take = |key: &str| fields.remove(key);
    Ok(Event {
        repo,
        kind,
        mr: take("MR"),
        prev: take("Prev"),
        head: take("Head"),
        base: take("Base"),
        target: take("Target"),
        topic: take("Topic"),
        reply_to: take("Reply-To"),
        file: take("File"),
        line,
        commit: take("Commit"),
        name: name.to_string(),
        ts,
        text: if text == format!("({})", kind.as_str()) {
            String::new()
        } else {
            text.to_string()
        },
    })
}

/// A merge request as its events leave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    /// ID of the opening event.
    pub id: String,
    /// Who opened it.
    pub author: KeyId,
    /// The author's display name.
    pub author_name: String,
    /// Title.
    pub title: String,
    /// Description.
    pub body: String,
    /// Branch to merge into.
    pub target: String,
    /// The author's branch name, when they gave one.
    pub topic: Option<String>,
    /// Currently proposed commit.
    pub head: String,
    /// Where it left the target branch.
    pub base: Option<String>,
    /// Closed without regard to whether it was merged.
    pub closed: bool,
    /// When it was opened.
    pub opened: u64,
    /// When the last event was written.
    pub updated: u64,
    /// Every event that counts, oldest first.
    pub events: Vec<VerifiedEvent>,
}

impl Thread {
    /// Build the thread for `mr` from its events. `may_manage` says whether a
    /// key is the root's or a maintainer's. Returns none without the opening
    /// event. Events their author was not entitled to write are left out.
    #[must_use]
    pub fn build(
        mr: &str,
        mut events: Vec<VerifiedEvent>,
        may_manage: impl Fn(KeyId) -> bool,
    ) -> Option<Self> {
        let open = events
            .iter()
            .find(|e| e.id == mr && e.event.kind == Kind::Open)?
            .clone();
        let author = open.signer;
        events.retain(|e| match e.event.kind {
            Kind::Open => e.id == mr,
            _ if e.event.mr.as_deref() != Some(mr) => false,
            Kind::Update => e.signer == author,
            Kind::Close | Kind::Reopen => e.signer == author || may_manage(e.signer),
            Kind::Comment | Kind::Approve | Kind::RequestChanges => true,
        });
        // The opening event first, whatever other clocks say.
        events
            .sort_by(|a, b| (a.id != mr, a.event.ts, &a.id).cmp(&(b.id != mr, b.event.ts, &b.id)));
        let mut thread = Self {
            id: mr.to_string(),
            author,
            author_name: open.event.name.clone(),
            title: open.event.title().to_string(),
            body: open.event.body().to_string(),
            target: open.event.target.clone()?,
            topic: open.event.topic.clone(),
            head: open.event.head.clone()?,
            base: open.event.base.clone(),
            closed: false,
            opened: open.event.ts,
            updated: open.event.ts,
            events: Vec::new(),
        };
        for item in &events {
            thread.updated = thread.updated.max(item.event.ts);
            match item.event.kind {
                Kind::Update => {
                    if let Some(head) = &item.event.head {
                        thread.head.clone_from(head);
                        thread.base.clone_from(&item.event.base);
                    }
                }
                Kind::Close => thread.closed = true,
                Kind::Reopen => thread.closed = false,
                _ => {}
            }
        }
        thread.events = events;
        Some(thread)
    }

    /// The latest review by each reviewer, other than the author's own.
    #[must_use]
    pub fn reviews(&self) -> Vec<&VerifiedEvent> {
        let mut latest = BTreeMap::new();
        for item in &self.events {
            if item.event.kind.is_review() && item.signer != self.author {
                latest.insert(item.signer, item);
            }
        }
        latest.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pq::HybridSigner;

    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn open(ts: u64) -> Event {
        let mut event = Event::new(RepoId::from_bytes([7; 32]), Kind::Open, "Ada", ts);
        event.head = Some(HEAD.into());
        event.base = Some(HEAD.into());
        event.target = Some("refs/heads/main".into());
        event.topic = Some("feature".into());
        event.text = "Add a thing\n\nWhy it helps.\n\nSwarm-Kind: close".into();
        event
    }

    fn signed(event: &Event, signer: &HybridSigner, id: u8) -> VerifiedEvent {
        let id = format!("{id:040x}");
        verify_event(&id, &event.sign(signer).unwrap()).unwrap()
    }

    #[test]
    fn round_trip_and_tamper() {
        let signer = HybridSigner::generate().unwrap();
        let event = open(5);
        let bytes = event.sign(&signer).unwrap();
        let read = verify_event(HEAD, &bytes).unwrap();
        assert_eq!(read.event, event);
        assert_eq!(read.signer, key_id(&signer.bundle()));
        // Text that looks like trailers stays text.
        assert_eq!(read.event.title(), "Add a thing");
        assert!(read.event.body().ends_with("Swarm-Kind: close"));

        let text = String::from_utf8(bytes).unwrap();
        for (from, to) in [
            ("Add a thing", "Add a think"),
            ("author Ada", "author Eve"),
            ("refs/heads/main", "refs/heads/evil"),
        ] {
            let changed = text.replacen(from, to, 1);
            assert_eq!(
                verify_event(HEAD, changed.as_bytes()),
                Err(ErrorCode::BadSignature),
                "{from}"
            );
        }
    }

    #[test]
    fn events_without_words_round_trip() {
        let signer = HybridSigner::generate().unwrap();
        let mut close = Event::new(RepoId::from_bytes([7; 32]), Kind::Close, "Ada", 9);
        close.mr = Some(HEAD.into());
        assert_eq!(signed(&close, &signer, 2).event, close);
    }

    #[test]
    fn bad_shapes_are_not_signed() {
        let signer = HybridSigner::generate().unwrap();
        let mut no_title = open(1);
        no_title.text.clear();
        let mut bad_target = open(1);
        bad_target.target = Some("refs/tags/v1".into());
        let mut escape = open(1);
        escape.text = "a\u{1b}[2J".into();
        let mut injected = open(1);
        injected.topic = Some("a\nSwarm-Kind: close".into());
        for event in [no_title, bad_target, escape, injected] {
            assert!(event.sign(&signer).is_err(), "{event:?}");
        }
        assert_eq!(clean_text("\r\n a\u{1b}\u{202e} \r\n\n"), " a");
        assert_eq!(clean_name("<x>\n"), "x");
    }

    #[test]
    fn only_the_author_updates_and_only_they_or_maintainers_close() {
        let author = HybridSigner::generate().unwrap();
        let other = HybridSigner::generate().unwrap();
        let maintainer = HybridSigner::generate().unwrap();
        let first = signed(&open(10), &author, 1);
        let mr = first.id.clone();
        let next = |kind: Kind, ts: u64| {
            let mut event = Event::new(first.event.repo, kind, "n", ts);
            event.mr = Some(mr.clone());
            if kind.proposes_code() {
                event.head = Some("2".repeat(40));
            }
            if matches!(kind, Kind::Approve | Kind::RequestChanges) {
                event.commit = Some(HEAD.into());
            }
            event
        };
        let events = vec![
            first.clone(),
            signed(&next(Kind::Update, 11), &other, 2),
            signed(&next(Kind::Close, 12), &other, 3),
            signed(&next(Kind::RequestChanges, 13), &other, 4),
            signed(&next(Kind::Approve, 14), &other, 5),
            // A clock set back does not move an event before the opening.
            signed(&next(Kind::Approve, 1), &author, 6),
        ];
        let key = key_id(&maintainer.bundle());
        let may = |signer: KeyId| signer == key;
        let thread = Thread::build(&mr, events.clone(), may).unwrap();
        assert_eq!(thread.head, HEAD);
        assert!(!thread.closed);
        assert_eq!(thread.events.first().unwrap().id, mr);
        let reviews = thread.reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].event.kind, Kind::Approve);

        let mut events = events;
        events.push(signed(&next(Kind::Update, 15), &author, 7));
        events.push(signed(&next(Kind::Close, 16), &maintainer, 8));
        let thread = Thread::build(&mr, events, may).unwrap();
        assert_eq!(thread.head, "2".repeat(40));
        assert!(thread.closed);
    }

    #[test]
    fn ref_names_round_trip() {
        let author = KeyId::from_bytes([0xab; 32]);
        let name = mr_ref(HEAD, author);
        assert_eq!(parse_mr_ref(&name), Some((HEAD.to_string(), author)));
        assert_eq!(parse_mr_ref("refs/mr/x/y"), None);
        assert_eq!(parse_mr_ref("refs/heads/main"), None);
    }
}
