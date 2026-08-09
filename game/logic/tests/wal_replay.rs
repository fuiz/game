//! Replaying a game's log has to land on the same state the live game reached.
//!
//! The Cloudflare object stops writing the whole game on every message and
//! appends an event instead, so a rebuilt game is the only thing standing
//! between an eviction and a corrupted quiz. These tests play games out for
//! real, then rebuild them from nothing but the log and compare the persisted
//! bytes.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use fuiz::{
    AlarmMessage, SyncMessage, UpdateMessage,
    fuiz::{
        common::SlideStateManager,
        config::{Fuiz, SlideState},
    },
    game::{
        Game, HostScreen, IncomingGhostMessage, IncomingHostMessage, IncomingMessage, IncomingPlayerMessage,
        IncomingUnassignedMessage, Options, Profanity, SlidePosition, State,
    },
    session::Tunnel,
    settings::Settings,
    tick::Tick,
    wal::{Entry, Event, Replay, apply},
    watcher::{Id, ValueKind},
};
use rustc_hash::FxHashSet;

/// Sends nowhere, but exists, so the game counts the watcher as connected.
#[derive(Clone, Copy)]
struct Wire;

impl Tunnel for Wire {
    fn send_message(&self, _message: &UpdateMessage) {}
    fn send_state(&self, _state: &SyncMessage) {}
    fn close(self) {}
}

/// Drives a game the way the Durable Object does: every mutation goes through
/// [`apply`], and every applied event is appended to the log.
struct Session {
    game: Game,
    log: Vec<Entry>,
    connected: Rc<RefCell<FxHashSet<Id>>>,
    seq: u64,
    /// Alarms the game asked for, drained by the caller to fire them back in.
    pending: Rc<RefCell<Vec<AlarmMessage>>>,
}

impl Session {
    fn new(config: Fuiz, options: Options, host: Id) -> Self {
        Self::with_settings(config, options, host, &Settings::default())
    }

    fn with_settings(config: Fuiz, options: Options, host: Id, settings: &Settings) -> Self {
        let connected: Rc<RefCell<FxHashSet<Id>>> = Rc::new(RefCell::new(FxHashSet::default()));
        connected.borrow_mut().insert(host);
        Self {
            game: Game::new(config, options, host, settings),
            log: Vec::new(),
            connected,
            seq: 0,
            pending: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn record(&mut self, event: Event) {
        match &event {
            Event::Joined(id) | Event::Rejoined(id) => {
                self.connected.borrow_mut().insert(*id);
            }
            Event::Left(id) => {
                self.connected.borrow_mut().remove(id);
            }
            Event::Received(..) | Event::Alarm(_) => {}
        }

        self.seq += 1;
        let tick = Tick::sample();

        let connected = Rc::clone(&self.connected);
        let pending = Rc::clone(&self.pending);
        // A refusal is a legitimate outcome to log; these games stay under the
        // player cap, so nothing here is expected to hit it.
        let _ = apply(
            &mut self.game,
            &event,
            tick,
            |message: AlarmMessage, _: Duration| pending.borrow_mut().push(message),
            |id: Id| connected.borrow().contains(&id).then_some(Wire),
        );

        self.log.push(Entry {
            seq: self.seq,
            tick,
            event,
        });
    }

    fn join(&mut self, index: usize) -> Id {
        let player = Id::new();
        self.record(Event::Joined(player));
        self.record(Event::Received(
            player,
            IncomingMessage::Unassigned(IncomingUnassignedMessage::NameRequest(format!("Player{index}"))),
        ));
        player
    }

    fn host_next(&mut self, host: Id) {
        let screen = host_screen(&self.game);
        self.record(Event::Received(
            host,
            IncomingMessage::Host(IncomingHostMessage::Next(screen)),
        ));
    }

    fn drain_alarms(&mut self) {
        loop {
            // Popped into a local so the borrow ends before `record` runs,
            // which needs the same cell to collect whatever this alarm schedules.
            let next = self.pending.borrow_mut().pop();
            let Some(message) = next else { break };
            self.record(Event::Alarm(message));
        }
    }

    /// Players who got as far as picking a name.
    fn watchers_named(&self) -> usize {
        self.game.watchers.specific_count(ValueKind::Player)
    }

    /// Rebuilds from an empty game plus the whole log.
    fn rebuild(&self, config: Fuiz, options: Options, host: Id) -> Game {
        self.rebuild_with(config, options, host, &Settings::default())
    }

    fn rebuild_with(&self, config: Fuiz, options: Options, host: Id, settings: &Settings) -> Game {
        let mut rebuilt = Game::new(config, options, host, settings);
        let mut replay = Replay::from_snapshot(0, FxHashSet::from_iter([host]));
        for entry in &self.log {
            assert!(replay.step(&mut rebuilt, entry), "entry {} should apply", entry.seq);
        }
        rebuilt
    }
}

fn bytes(game: &Game) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(game, &mut out).expect("game serializes");
    out
}

fn host_screen(game: &Game) -> HostScreen {
    match &game.state {
        State::WaitingScreen | State::TeamDisplay => HostScreen::Lobby,
        State::Leaderboard(index) => HostScreen::Leaderboard { index: *index },
        State::Done => HostScreen::Summary,
        State::Slide(current) => {
            let index = current.index;
            HostScreen::Slide(match &current.state {
                SlideState::MultipleChoice(s) => SlidePosition::MultipleChoice {
                    index,
                    phase: s.state(),
                },
                SlideState::TypeAnswer(s) => SlidePosition::TypeAnswer {
                    index,
                    phase: s.state(),
                },
                SlideState::Order(s) => SlidePosition::Order {
                    index,
                    phase: s.state(),
                },
                SlideState::Slider(s) => SlidePosition::Slider {
                    index,
                    phase: s.state(),
                },
                SlideState::Scale(s) => SlidePosition::Scale {
                    index,
                    phase: s.state(),
                },
                SlideState::Poll(s) => SlidePosition::Poll {
                    index,
                    phase: s.state(),
                },
                SlideState::Pin(s) => SlidePosition::Pin {
                    index,
                    phase: s.state(),
                },
                SlideState::FreeText(s) => SlidePosition::FreeText {
                    index,
                    phase: s.state(),
                },
                SlideState::Brainstorm(s) => SlidePosition::Brainstorm {
                    index,
                    phase: s.state(),
                },
                SlideState::InfoSlide(s) => SlidePosition::InfoSlide {
                    index,
                    phase: s.state(),
                },
            })
        }
    }
}

fn quiz(slides: usize) -> Fuiz {
    let slide = r#"{ "MultipleChoice": { "title": "Which of these is the capital city?",
        "introduce_question": 5000, "time_limit": 30000, "points_awarded": 1000,
        "answers": [
          { "content": { "Text": "Amsterdam" }, "correct": true },
          { "content": { "Text": "Rotterdam" }, "correct": false },
          { "content": { "Text": "The Hague" }, "correct": false },
          { "content": { "Text": "Utrecht" }, "correct": false } ] } }"#;
    Fuiz {
        title: "Replay quiz".to_string(),
        slides: serde_json::from_str(&format!("[{}]", vec![slide; slides].join(","))).expect("slides deserialize"),
    }
}

fn options() -> Options {
    Options::default().with_profanity(Profanity::Allow)
}

/// Plays a whole game, answers and all, and rebuilds it from the log.
#[test]
fn a_replayed_game_matches_the_game_it_replays() {
    let host = Id::new();
    let mut session = Session::new(quiz(3), options(), host);

    let players: Vec<Id> = (0..8).map(|index| session.join(index)).collect();

    // Start, then walk every slide: answer whenever the slide is taking them.
    session.host_next(host);
    session.drain_alarms();

    for round in 0..3 {
        for (index, player) in players.iter().enumerate() {
            session.record(Event::Received(
                *player,
                IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer((index + round) % 4)),
            ));
        }
        session.drain_alarms();
        session.host_next(host);
        session.drain_alarms();
        session.host_next(host);
        session.drain_alarms();
    }

    let rebuilt = session.rebuild(quiz(3), options(), host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "a game rebuilt from its log should serialize identically to the live one"
    );
}

/// Scores are the timing-sensitive part: a player who answered a second in
/// gets fewer points than one who answered instantly, and a replay has to
/// reproduce the gap rather than re-time everyone at replay speed.
#[test]
fn replay_preserves_time_based_scores() {
    let host = Id::new();
    let mut session = Session::new(quiz(1), options(), host);
    let players: Vec<Id> = (0..4).map(|index| session.join(index)).collect();

    session.host_next(host);
    session.drain_alarms();

    // Answer, then let real time pass between submissions so the recorded
    // stamps genuinely differ.
    for player in &players {
        std::thread::sleep(Duration::from_millis(15));
        session.record(Event::Received(
            *player,
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(0)),
        ));
    }

    session.drain_alarms();
    session.host_next(host);
    session.drain_alarms();

    let rebuilt = session.rebuild(quiz(1), options(), host);

    let live = bytes(&session.game);
    assert_eq!(
        bytes(&rebuilt),
        live,
        "replayed scores should match the live ones, stamps and all"
    );
    // Guard against the comparison passing because nobody scored at all.
    assert!(
        matches!(session.game.state, State::Leaderboard(_) | State::Slide(_)),
        "the game should have reached scoring"
    );
}

/// A watcher who drops changes what the game does, because the tunnel finder
/// is what tells it who is still there. Replay reconstructs that from the log.
#[test]
fn replay_reproduces_disconnects() {
    let host = Id::new();
    let mut session = Session::new(quiz(2), options(), host);
    let players: Vec<Id> = (0..6).map(|index| session.join(index)).collect();

    session.host_next(host);
    session.drain_alarms();

    session.record(Event::Received(
        players[0],
        IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(0)),
    ));

    // Two players drop, one of whom had already answered, and one comes back.
    session.record(Event::Left(players[0]));
    session.record(Event::Left(players[1]));
    session.record(Event::Rejoined(players[1]));

    for player in &players[2..] {
        session.record(Event::Received(
            *player,
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(1)),
        ));
    }

    session.drain_alarms();
    session.host_next(host);
    session.drain_alarms();

    let rebuilt = session.rebuild(quiz(2), options(), host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "replay should reproduce the state disconnects left behind"
    );
}

/// Team formation shuffles players and mints team ids, the one place the game
/// draws randomness. The seed rides along in the entry, so a replay forms the
/// same teams rather than a fresh random arrangement.
#[test]
fn replay_reproduces_random_team_formation() {
    let host = Id::new();
    // Team options carry private fields, so build them the way a request does.
    let teamed: Options = serde_json::from_str(
        r#"{ "random_names": null, "show_answers": false, "no_leaderboard": false,
             "teams": { "size": 3, "assign_random": true }, "profanity": "Allow" }"#,
    )
    .expect("team options deserialize");

    let mut session = Session::new(quiz(1), teamed, host);
    for index in 0..9 {
        session.join(index);
    }

    // Starting the game is what finalizes teams.
    session.host_next(host);
    session.drain_alarms();

    let rebuilt = session.rebuild(quiz(1), teamed, host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "replayed team assignment should match the live draw"
    );
}

/// A snapshot names the prefix it already covers. Re-feeding entries from
/// before it, which is what happens if the object dies between writing a
/// snapshot and dropping the entries it covers, must not double-apply them.
#[test]
fn replay_skips_entries_a_snapshot_already_covers() {
    let host = Id::new();
    let mut session = Session::new(quiz(1), options(), host);
    let players: Vec<Id> = (0..4).map(|index| session.join(index)).collect();

    session.host_next(host);
    session.drain_alarms();
    for player in &players {
        session.record(Event::Received(
            *player,
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(2)),
        ));
    }

    // Rebuild in two halves: replay the first half, "snapshot" there, then
    // hand the second replay the whole log including the covered prefix.
    let split = session.log.len() / 2;
    let mut partial = Game::new(quiz(1), options(), host, &Settings::default());
    let mut replay = Replay::from_snapshot(0, FxHashSet::from_iter([host]));
    for entry in &session.log[..split] {
        replay.step(&mut partial, entry);
    }

    let mut resumed = Replay::from_snapshot(replay.applied(), replay.connected().clone());
    let mut skipped = 0;
    for entry in &session.log {
        if !resumed.step(&mut partial, entry) {
            skipped += 1;
        }
    }

    assert_eq!(skipped, split, "every covered entry should have been skipped");
    assert_eq!(
        bytes(&partial),
        bytes(&session.game),
        "resuming from a snapshot should reach the same state as replaying it all"
    );
}

/// With random names switched on, every join draws words from a list, and a
/// collision redraws. That is the randomness players actually hit, once per
/// join, so it has to come back the same on replay.
#[test]
fn replay_reproduces_random_player_names() {
    let host = Id::new();
    let named: Options = serde_json::from_str(
        r#"{ "random_names": { "Petname": 2 }, "show_answers": false, "no_leaderboard": false,
             "teams": null, "profanity": "Allow" }"#,
    )
    .expect("random-name options deserialize");

    let mut session = Session::new(quiz(1), named, host);
    for _ in 0..20 {
        let player = Id::new();
        session.record(Event::Joined(player));
    }

    let rebuilt = session.rebuild(quiz(1), named, host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "replayed players should carry the names the live draw gave them"
    );
}

/// A refused join is still logged, and replay marks the id connected before
/// discovering the game turned it away. That leftover must not reach anything
/// the game persists: a watcher the game never admitted is in no watcher set,
/// so nothing the tunnel finder is asked about can depend on it.
#[test]
fn a_refused_join_leaves_nothing_behind() {
    let host = Id::new();
    let mut capped = Settings::default();
    capped.fuiz.max_player_count = 4; // the host plus three players

    let mut session = Session::with_settings(quiz(1), options(), host, &capped);
    for index in 0..3 {
        session.join(index);
    }

    // A fourth player is over the cap. The event is logged either way.
    let refused = Id::new();
    session.record(Event::Joined(refused));

    assert_eq!(
        session.game.watchers.specific_count(ValueKind::Unassigned) + session.watchers_named(),
        3,
        "the fourth join should have been turned away, or this tests nothing"
    );

    session.host_next(host);
    session.drain_alarms();

    let rebuilt = session.rebuild_with(quiz(1), options(), host, &capped);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "a join the game refused should replay to the same state"
    );
}

/// A kicked player is dropped from the watcher set entirely, so the id the
/// replay still counts as connected can no longer be reached through it.
#[test]
fn replay_reproduces_a_kick() {
    let host = Id::new();
    let mut session = Session::new(quiz(2), options(), host);
    let players: Vec<Id> = (0..4).map(|index| session.join(index)).collect();

    session.host_next(host);
    session.drain_alarms();

    session.record(Event::Received(
        players[0],
        IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(1)),
    ));
    let before = session.watchers_named();
    session.record(Event::Received(
        host,
        IncomingMessage::Host(IncomingHostMessage::Kick("Player1".to_string())),
    ));
    assert_eq!(
        session.watchers_named(),
        before - 1,
        "the kick should have removed a player, or this tests nothing"
    );

    for player in &players[2..] {
        session.record(Event::Received(
            *player,
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(0)),
        ));
    }

    session.drain_alarms();
    session.host_next(host);
    session.drain_alarms();

    let rebuilt = session.rebuild(quiz(2), options(), host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "a kick should replay to the same state"
    );
}

/// The ghost handshake a returning client actually sends.
#[test]
fn replay_handles_ghost_reconnects() {
    let host = Id::new();
    let mut session = Session::new(quiz(1), options(), host);
    let player = session.join(0);

    session.record(Event::Left(player));
    session.record(Event::Rejoined(player));
    session.record(Event::Received(
        player,
        IncomingMessage::Ghost(IncomingGhostMessage::ClaimId(player)),
    ));

    let rebuilt = session.rebuild(quiz(1), options(), host);

    assert_eq!(
        bytes(&rebuilt),
        bytes(&session.game),
        "a ghost reconnect should replay cleanly"
    );
}
