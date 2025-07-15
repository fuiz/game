//! Core game logic and state management
//!
//! This module contains the main game struct and logic for managing
//! a Fuiz game session, including player management, question flow,
//! scoring, team formation, and real-time communication with all
//! connected participants.

use std::{collections::HashSet, fmt::Debug};

use garde::Validate;
use heck::ToTitleCase;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::{
    fuiz::{config::CurrentSlide, order, type_answer},
    watcher::Value,
};

use super::{
    AlarmMessage, TruncatedVec,
    fuiz::{config::Fuiz, multiple_choice},
    leaderboard::{Leaderboard, ScoreMessage},
    names::{self, Names},
    session::Tunnel,
    teams::{self, TeamManager},
    watcher::{self, Id, PlayerValue, ValueKind, Watchers},
};

/// Represents the current phase or state of the game
///
/// The game progresses through different states, from waiting for players
/// to join, through individual questions, to showing results and completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum State {
    /// Waiting screen showing current players before the game starts
    WaitingScreen,
    /// Team formation screen (only shown in team games)
    TeamDisplay,
    /// Currently displaying a specific slide/question
    Slide(Box<CurrentSlide>),
    /// Showing the leaderboard after a question (with index)
    Leaderboard(usize),
    /// Game has completed
    Done,
}

/// Configuration options for team-based games
///
/// This struct defines how teams are formed and managed within a game session.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Validate)]
pub struct TeamOptions {
    /// Maximum initial size for teams
    #[garde(range(min = 1, max = 5))]
    size: usize,
    /// Whether to assign players to random teams or let them choose preferences
    #[garde(skip)]
    assign_random: bool,
}

/// Defines the style of automatically generated player names
///
/// When random names are enabled, this enum determines what type of
/// names are generated for players who don't choose their own names.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Validate)]
pub enum NameStyle {
    /// Roman-style names (praenomen + nomen, optionally + cognomen)
    Roman(#[garde(range(min = 2, max = 3))] usize),
    /// Pet-style names (adjective + animal combinations)
    Petname(#[garde(range(min = 2, max = 3))] usize),
}

impl Default for NameStyle {
    /// Default name style is Petname with 2 words
    fn default() -> Self {
        Self::Petname(2)
    }
}

impl NameStyle {
    /// Generates a random name according to this style
    ///
    /// # Returns
    ///
    /// A randomly generated name as a String.
    pub fn get_name(&self) -> String {
        match self {
            Self::Roman(count) => romanname::romanname(romanname::NameConfig {
                praenomen: *count > 2,
            }),
            Self::Petname(count) => loop {
                if let Some(name) = petname::petname(*count as u8, " ") {
                    return name;
                }
            },
        }
        .to_title_case()
    }
}

/// Global configuration options for the game session
///
/// These options affect the overall behavior of the game, including
/// name generation, answer visibility, leaderboard display, and team formation.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, Validate)]
pub struct Options {
    /// Style for automatically generated player names (None means players choose their own)
    #[garde(dive)]
    random_names: Option<NameStyle>,
    /// Whether to show correct answers on player devices after questions
    #[garde(skip)]
    show_answers: bool,
    /// Whether to skip showing leaderboards between questions
    #[garde(skip)]
    no_leaderboard: bool,
    /// Team configuration (None means individual play)
    #[garde(dive)]
    teams: Option<TeamOptions>,
}

/// The main game session struct
///
/// This struct represents a complete Fuiz game session, managing all
/// aspects of the game including participant connections, question flow,
/// scoring, team management, and real-time communication.
#[derive(Serialize, Deserialize)]
pub struct Game {
    /// The Fuiz configuration containing all questions and settings
    fuiz_config: Fuiz,
    /// Manager for all connected participants (players, hosts, unassigned)
    pub watchers: Watchers,
    /// Name assignments and validation for players
    names: Names,
    /// Scoring and leaderboard management
    pub leaderboard: Leaderboard,
    /// Current phase/state of the game
    pub state: State,
    /// Game configuration options
    options: Options,
    /// Whether the game is locked to new participants
    locked: bool,
    /// Team formation and management (if teams are enabled)
    team_manager: Option<TeamManager>,
}

impl Debug for Game {
    /// Custom debug implementation that avoids printing large amounts of data
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Game")
            .field("fuiz", &self.fuiz_config)
            .finish_non_exhaustive()
    }
}

/// Messages received from different types of participants
///
/// This enum categorizes incoming messages based on the sender's role,
/// ensuring that only appropriate messages are processed from each
/// participant type.
#[derive(Debug, Deserialize, Clone)]
pub enum IncomingMessage {
    /// Messages from disconnected clients trying to reconnect
    Ghost(IncomingGhostMessage),
    /// Messages from the game host
    Host(IncomingHostMessage),
    /// Messages from unassigned connections (not yet players)
    Unassigned(IncomingUnassignedMessage),
    /// Messages from active players
    Player(IncomingPlayerMessage),
}

impl IncomingMessage {
    /// Validates that a message matches the sender's participant type
    ///
    /// This ensures that participants can only send messages appropriate
    /// for their current role in the game session.
    ///
    /// # Arguments
    ///
    /// * `sender_kind` - The type of participant sending the message
    ///
    /// # Returns
    ///
    /// `true` if the message type matches the sender type, `false` otherwise
    fn follows(&self, sender_kind: ValueKind) -> bool {
        matches!(
            (self, sender_kind),
            (IncomingMessage::Host(_), ValueKind::Host)
                | (IncomingMessage::Player(_), ValueKind::Player)
                | (IncomingMessage::Unassigned(_), ValueKind::Unassigned)
        )
    }
}

/// Messages that can be sent by active players
#[derive(Debug, Deserialize, Clone)]
pub enum IncomingPlayerMessage {
    /// Answer selected by index (for multiple choice questions)
    IndexAnswer(usize),
    /// Text answer submitted (for type answer questions)
    StringAnswer(String),
    /// Array of strings submitted (for order questions)
    StringArrayAnswer(Vec<String>),
    /// Team preference selection (during team formation)
    ChooseTeammates(Vec<String>),
}

/// Messages that can be sent by unassigned connections
#[derive(Debug, Deserialize, Clone)]
pub enum IncomingUnassignedMessage {
    /// Request to set a specific name and become a player
    NameRequest(String),
}

/// Messages that can be sent by disconnected clients trying to reconnect
#[derive(Debug, Deserialize, Clone)]
pub enum IncomingGhostMessage {
    /// Request a new ID assignment
    DemandId,
    /// Attempt to reclaim a specific existing ID
    ClaimId(Id),
}

/// Messages that can be sent by the game host
#[derive(Debug, Deserialize, Clone, Copy)]
pub enum IncomingHostMessage {
    /// Advance to the next slide/question
    Next,
    /// Jump to a specific slide by index
    Index(usize),
    /// Lock or unlock the game to new participants
    Lock(bool),
}

/// Update messages sent to participants about game state changes
///
/// These messages inform participants about changes that affect their
/// view or interaction with the game.
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum UpdateMessage {
    /// Assign a unique ID to a participant
    IdAssign(Id),
    /// Update the waiting screen with current players
    WaitingScreen(TruncatedVec<String>),
    /// Update the team display screen
    TeamDisplay(TruncatedVec<String>),
    /// Prompt the participant to choose a name
    NameChoose,
    /// Confirm a name assignment
    NameAssign(String),
    /// Report an error with name validation
    NameError(names::Error),
    /// Send leaderboard information
    Leaderboard {
        /// The leaderboard data to display
        leaderboard: LeaderboardMessage,
    },
    /// Send individual score information
    Score {
        /// The player's score information
        score: Option<ScoreMessage>,
    },
    /// Send game summary information
    Summary(SummaryMessage),
    /// Inform player to find a team (team games only)
    FindTeam(String),
    /// Prompt for teammate selection during team formation
    ChooseTeammates {
        /// Maximum number of teammates that can be selected
        max_selection: usize,
        /// Available players with their current selection status: (name, is_selected)
        available: Vec<(String, bool)>,
    },
}

/// Sync messages sent to participants to synchronize their view with game state
///
/// These messages are sent when participants connect or when their view
/// needs to be completely synchronized with the current game state.
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum SyncMessage {
    /// Sync waiting screen with current players
    WaitingScreen(TruncatedVec<String>),
    /// Sync team display screen
    TeamDisplay(TruncatedVec<String>),
    /// Sync leaderboard view with position information
    Leaderboard {
        /// Current slide index
        index: usize,
        /// Total number of slides
        count: usize,
        /// The leaderboard data to display
        leaderboard: LeaderboardMessage,
    },
    /// Sync individual score view with position information
    Score {
        /// Current slide index
        index: usize,
        /// Total number of slides
        count: usize,
        /// The player's score information
        score: Option<ScoreMessage>,
    },
    /// Sync metadata about the game state
    Metainfo(MetainfoMessage),
    /// Sync game summary information
    Summary(SummaryMessage),
    /// Participant is not allowed to join
    NotAllowed,
    /// Sync team finding information
    FindTeam(String),
    /// Sync teammate selection options
    ChooseTeammates {
        /// Maximum number of teammates that can be selected
        max_selection: usize,
        /// Available players with their current selection status: (name, is_selected)
        available: Vec<(String, bool)>,
    },
}

/// Summary information sent at the end of the game
///
/// This enum provides different views of the game results depending
/// on whether the recipient is a player or the host.
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum SummaryMessage {
    /// Summary for individual players
    Player {
        /// Player's final score information
        score: Option<ScoreMessage>,
        /// Points earned on each question
        points: Vec<u64>,
        /// The game configuration that was played
        config: Fuiz,
    },
    /// Summary for the game host with detailed statistics
    Host {
        /// Statistics for each question: (correct_count, total_count)
        stats: Vec<(usize, usize)>,
        /// Total number of players who participated
        player_count: usize,
        /// The game configuration that was played
        config: Fuiz,
        /// Game options that were used
        options: Options,
    },
}

/// Metadata information about the game state
///
/// This provides contextual information that participants need
/// to understand their current status and available actions.
#[derive(Debug, Serialize, Clone)]
pub enum MetainfoMessage {
    /// Information for the game host
    Host {
        /// Whether the game is locked to new participants
        locked: bool,
    },
    /// Information for players
    Player {
        /// Player's current total score
        score: u64,
        /// Whether answers will be shown after questions
        show_answers: bool,
    },
}

/// Leaderboard data structure for display
///
/// Contains both current standings and previous round standings
/// for comparison and ranking visualization.
#[derive(Debug, Serialize, Clone)]
pub struct LeaderboardMessage {
    /// Current leaderboard standings
    pub current: TruncatedVec<(String, u64)>,
    /// Previous round's standings for comparison
    pub prior: TruncatedVec<(String, u64)>,
}

// Convenience methods
impl Game {
    /// Sets the current game state
    ///
    /// # Arguments
    ///
    /// * `game_state` - The new state to transition to
    fn set_state(&mut self, game_state: State) {
        self.state = game_state;
    }

    /// Gets the score information for a specific watcher
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher to get score for
    ///
    /// # Returns
    ///
    /// Score information if the watcher has a score, otherwise `None`
    fn score(&self, watcher_id: Id) -> Option<ScoreMessage> {
        self.leaderboard.score(self.leaderboard_id(watcher_id))
    }

    /// Gets the leaderboard ID for a player (team ID if in team mode, player ID otherwise)
    ///
    /// In team games, this returns the team ID so that team scores are tracked.
    /// In individual games, this returns the player ID directly.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player's individual ID
    ///
    /// # Returns
    ///
    /// The ID to use for leaderboard tracking (team or individual)
    pub fn leaderboard_id(&self, player_id: Id) -> Id {
        match &self.team_manager {
            Some(team_manager) => team_manager.get_team(player_id).unwrap_or(player_id),
            None => player_id,
        }
    }

    /// Creates a teammate selection message for team formation
    ///
    /// This generates the message shown to players during team formation,
    /// allowing them to select their preferred teammates from available players.
    ///
    /// # Arguments
    ///
    /// * `watcher` - The ID of the player who needs to choose teammates
    /// * `team_manager` - The team manager containing preference data
    /// * `tunnel_finder` - Function to find active communication tunnels
    ///
    /// # Returns
    ///
    /// An UpdateMessage with teammate selection options
    fn choose_teammates_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher: Id,
        team_manager: &TeamManager,
        tunnel_finder: F,
    ) -> UpdateMessage {
        let pref: HashSet<_> = team_manager
            .get_preferences(watcher)
            .unwrap_or_default()
            .into_iter()
            .collect();
        UpdateMessage::ChooseTeammates {
            max_selection: team_manager.optimal_size,
            available: self
                .watchers
                .specific_vec(ValueKind::Player, tunnel_finder)
                .into_iter()
                .filter_map(|(id, _, _)| Some((id, self.watchers.get_name(id)?)))
                .map(|(id, name)| (name, pref.contains(&id)))
                .collect(),
        }
    }

    /// Generates a list of player names for the waiting screen
    ///
    /// Creates a truncated list of player names to display on the waiting
    /// screen or team display. In team games, may show team names instead
    /// of individual player names depending on the current game state.
    ///
    /// # Arguments
    ///
    /// * `tunnel_finder` - Function to find active communication tunnels
    ///
    /// # Returns
    ///
    /// A TruncatedVec containing player names with overflow information
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn waiting_screen_names<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        tunnel_finder: F,
    ) -> TruncatedVec<String> {
        const LIMIT: usize = 50;

        if let Some(team_manager) = &self.team_manager {
            if matches!(self.state, State::TeamDisplay) {
                return team_manager.team_names().unwrap_or_default();
            }
        }

        let player_names = self
            .watchers
            .specific_vec(ValueKind::Player, tunnel_finder)
            .into_iter()
            .filter_map(|(_, _, x)| match x {
                Value::Player(player_value) => Some(player_value.name().to_owned()),
                _ => None,
            })
            .unique();

        TruncatedVec::new(
            player_names,
            LIMIT,
            self.watchers.specific_count(ValueKind::Player),
        )
    }

    /// Creates a leaderboard message with current and previous standings
    ///
    /// Generates a leaderboard message containing both the current standings
    /// and the previous round's standings for comparison. Player/team IDs
    /// are converted to display names for the client interface.
    ///
    /// # Returns
    ///
    /// A LeaderboardMessage with current and prior standings
    fn leaderboard_message(&self) -> LeaderboardMessage {
        let [current, prior] = self.leaderboard.last_two_scores_descending();

        let id_map = |i| self.names.get_name(&i).unwrap_or("Unknown".to_owned());

        let id_score_map = |(id, s)| (id_map(id), s);

        LeaderboardMessage {
            current: current.map(id_score_map),
            prior: prior.map(id_score_map),
        }
    }
}

impl Game {
    /// Creates a new game instance with the provided configuration
    ///
    /// Initializes a new Fuiz game session with the given quiz configuration,
    /// game options, and host identifier. Sets up the initial state, scoring
    /// system, name management, and team configuration if teams are enabled.
    ///
    /// # Arguments
    ///
    /// * `fuiz` - The quiz configuration containing questions and settings
    /// * `options` - Game options including team settings, name generation, etc.
    /// * `host_id` - Unique identifier for the game host
    ///
    /// # Returns
    ///
    /// A new Game instance ready to accept players and begin
    ///
    /// # Examples
    ///
    /// ```rust
    /// use fuiz::game::{Game, Options};
    /// use fuiz::fuiz::config::Fuiz;
    /// use fuiz::watcher::Id;
    ///
    /// let host_id = Id::new();
    /// let options = Options::default();
    /// let fuiz_config = Fuiz::default();
    /// let game = Game::new(fuiz_config, options, host_id);
    /// ```
    pub fn new(fuiz: Fuiz, options: Options, host_id: Id) -> Self {
        Self {
            fuiz_config: fuiz,
            watchers: Watchers::with_host_id(host_id),
            names: Names::default(),
            leaderboard: Leaderboard::default(),
            state: State::WaitingScreen,
            options,
            team_manager: options.teams.map(
                |TeamOptions {
                     size,
                     assign_random,
                 }| {
                    TeamManager::new(
                        size,
                        assign_random,
                        options.random_names.unwrap_or_default(),
                    )
                },
            ),
            locked: false,
        }
    }

    /// Starts the game or progresses to the next phase
    ///
    /// This method handles the transition from waiting/team formation to the first slide,
    /// or manages team formation when teams are enabled. It sets up the initial slide
    /// state and begins the question flow.
    ///
    /// # Arguments
    ///
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    pub fn play<T: Tunnel, F: Fn(Id) -> Option<T>, S: FnMut(AlarmMessage, web_time::Duration)>(
        &mut self,
        schedule_message: S,
        tunnel_finder: F,
    ) {
        if let Some(slide) = self.fuiz_config.slides.first() {
            if let Some(team_manager) = &mut self.team_manager {
                if matches!(self.state, State::WaitingScreen) {
                    team_manager.finalize(&mut self.watchers, &mut self.names, &tunnel_finder);
                    self.state = State::TeamDisplay;
                    self.watchers.announce_with(
                        |id, kind| {
                            Some(match kind {
                                ValueKind::Player => UpdateMessage::FindTeam(
                                    self.watchers.get_team_name(id).unwrap_or_default(),
                                )
                                .into(),
                                ValueKind::Host => UpdateMessage::TeamDisplay(
                                    team_manager.team_names().unwrap_or_default(),
                                )
                                .into(),
                                ValueKind::Unassigned => {
                                    return None;
                                }
                            })
                        },
                        &tunnel_finder,
                    );
                    return;
                }
            }

            let mut current_slide = CurrentSlide {
                index: 0,
                state: slide.to_state(),
            };

            current_slide.state.play(
                self.team_manager.as_ref(),
                &self.watchers,
                schedule_message,
                tunnel_finder,
                0,
                self.fuiz_config.len(),
            );

            self.set_state(State::Slide(Box::new(current_slide)));
        } else {
            self.announce_summary(tunnel_finder);
        }
    }

    /// Marks the current slide as done and transitions to the next phase
    ///
    /// This method handles the completion of a slide, either advancing to the
    /// leaderboard (if enabled) or directly to the next slide. If all slides
    /// are complete, it announces the game summary.
    ///
    /// # Arguments
    ///
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    pub fn finish_slide<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(AlarmMessage, web_time::Duration),
    >(
        &mut self,
        schedule_message: S,
        tunnel_finder: F,
    ) {
        if let State::Slide(current_slide) = &self.state {
            if self.options.no_leaderboard {
                let next_index = current_slide.index + 1;
                if let Some(next_slide) = self.fuiz_config.slides.get(next_index) {
                    let mut state = next_slide.to_state();

                    state.play(
                        self.team_manager.as_ref(),
                        &self.watchers,
                        schedule_message,
                        &tunnel_finder,
                        next_index,
                        self.fuiz_config.len(),
                    );

                    self.state = State::Slide(Box::new(CurrentSlide {
                        index: next_index,
                        state,
                    }));
                } else {
                    self.announce_summary(tunnel_finder);
                }
            } else {
                self.set_state(State::Leaderboard(current_slide.index));

                let leaderboard_message = self.leaderboard_message();

                self.watchers.announce_with(
                    |watcher_id, watcher_kind| {
                        Some(match watcher_kind {
                            ValueKind::Host => UpdateMessage::Leaderboard {
                                leaderboard: leaderboard_message.clone(),
                            }
                            .into(),
                            ValueKind::Player => UpdateMessage::Score {
                                score: self.score(watcher_id),
                            }
                            .into(),
                            ValueKind::Unassigned => return None,
                        })
                    },
                    tunnel_finder,
                );
            }
        }
    }

    /// Sends the final game summary to all participants
    ///
    /// This method transitions the game to the Done state and sends appropriate
    /// summary messages to hosts and players. Hosts receive detailed statistics
    /// while players receive their individual scores and points breakdown.
    ///
    /// # Arguments
    ///
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn announce_summary<T: Tunnel, F: Fn(Id) -> Option<T>>(&mut self, tunnel_finder: F) {
        self.state = State::Done;

        self.watchers.announce_with(
            |id, vk| match vk {
                ValueKind::Host => Some(
                    UpdateMessage::Summary({
                        let (player_count, stats) =
                            self.leaderboard.host_summary(!self.options.no_leaderboard);

                        SummaryMessage::Host {
                            stats,
                            player_count,
                            config: self.fuiz_config.clone(),
                            options: self.options,
                        }
                    })
                    .into(),
                ),
                ValueKind::Player => Some(
                    UpdateMessage::Summary(SummaryMessage::Player {
                        score: if self.options.no_leaderboard {
                            None
                        } else {
                            self.score(id)
                        },
                        points: self
                            .leaderboard
                            .player_summary(self.leaderboard_id(id), !self.options.no_leaderboard),
                        config: self.fuiz_config.clone(),
                    })
                    .into(),
                ),
                ValueKind::Unassigned => None,
            },
            tunnel_finder,
        );
    }

    /// Marks the game as done and disconnects all players
    ///
    /// This method finalizes the game session by setting the state to Done
    /// and removing all participant sessions, effectively ending the game.
    ///
    /// # Arguments
    ///
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn mark_as_done<T: Tunnel, F: Fn(Id) -> Option<T>>(&mut self, tunnel_finder: F) {
        self.state = State::Done;

        let watchers = self
            .watchers
            .vec(&tunnel_finder)
            .iter()
            .map(|(x, _, _)| *x)
            .collect_vec();

        for watcher in watchers {
            self.watchers
                .remove_watcher_session(&watcher, &tunnel_finder);
        }
    }

    /// Sends metadata information to a player about the game
    ///
    /// This method sends game options and player-specific information like
    /// current score and whether answers will be shown after questions.
    ///
    /// # Arguments
    ///
    /// * `watcher` - ID of the player to send metadata to
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn update_player_with_options<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher: Id,
        tunnel_finder: F,
    ) {
        self.watchers.send_state(
            &SyncMessage::Metainfo(MetainfoMessage::Player {
                score: self.score(watcher).map_or(0, |x| x.points),
                show_answers: self.options.show_answers,
            })
            .into(),
            watcher,
            tunnel_finder,
        );
    }

    /// Initiates interactions with an unassigned player
    ///
    /// This method handles the initial setup for new participants, either
    /// automatically assigning them a random name (if enabled) or prompting
    /// them to choose their own name.
    ///
    /// # Arguments
    ///
    /// * `watcher` - ID of the unassigned participant
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn handle_unassigned<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher: Id,
        tunnel_finder: F,
    ) {
        if let Some(name_style) = self.options.random_names {
            loop {
                let name = name_style.get_name();

                if self
                    .assign_player_name(watcher, &name, &tunnel_finder)
                    .is_ok()
                {
                    break;
                }
            }
        } else {
            self.watchers
                .send_message(&UpdateMessage::NameChoose.into(), watcher, tunnel_finder);
        }
    }

    /// Assigns a name to a player and converts them from unassigned to player
    ///
    /// This method validates the name, assigns it to the participant, and
    /// updates their status from unassigned to player. It handles name
    /// validation and uniqueness checking.
    ///
    /// # Arguments
    ///
    /// * `watcher` - ID of the participant to assign a name to
    /// * `name` - The name to assign to the participant
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the name was assigned successfully
    /// * `Err(names::Error)` if the name is invalid or already taken
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn assign_player_name<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher: Id,
        name: &str,
        tunnel_finder: F,
    ) -> Result<(), names::Error> {
        let name = self.names.set_name(watcher, name)?;

        self.watchers.update_watcher_value(
            watcher,
            Value::Player(watcher::PlayerValue::Individual { name: name.clone() }),
        );

        self.update_player_with_name(watcher, &name, tunnel_finder);

        Ok(())
    }

    /// Sends messages to the player about their newly assigned name
    ///
    /// This method notifies the player of their name assignment, handles team
    /// assignment if teams are enabled, and updates the waiting screen for other
    /// participants. It also sends the current game state to the newly named player.
    ///
    /// # Arguments
    ///
    /// * `watcher` - ID of the player whose name was assigned
    /// * `name` - The name that was assigned to the player
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn update_player_with_name<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher: Id,
        name: &str,
        tunnel_finder: F,
    ) {
        if let Some(team_manager) = &mut self.team_manager {
            if let Some(name) = team_manager.add_player(watcher, &mut self.watchers) {
                self.watchers.send_message(
                    &UpdateMessage::FindTeam(name).into(),
                    watcher,
                    &tunnel_finder,
                );
            }
        }

        self.watchers.send_message(
            &UpdateMessage::NameAssign(name.to_string()).into(),
            watcher,
            &tunnel_finder,
        );

        self.update_player_with_options(watcher, &tunnel_finder);

        if !name.is_empty() {
            // Announce to others of user joining
            if matches!(self.state, State::WaitingScreen) {
                if let Some(team_manager) = &self.team_manager {
                    if !team_manager.is_random_assignments() {
                        self.watchers.announce_with(
                            |id, value| match value {
                                ValueKind::Player => Some(
                                    self.choose_teammates_message(id, team_manager, &tunnel_finder)
                                        .into(),
                                ),
                                _ => None,
                            },
                            &tunnel_finder,
                        );
                    }
                }

                self.watchers.announce_specific(
                    ValueKind::Host,
                    &UpdateMessage::WaitingScreen(self.waiting_screen_names(&tunnel_finder)).into(),
                    &tunnel_finder,
                );
            }
        }

        self.watchers.send_state(
            &self.state_message(watcher, ValueKind::Player, &tunnel_finder),
            watcher,
            tunnel_finder,
        );
    }

    // Network

    /// Adds a new unassigned participant to the game
    ///
    /// This method registers a new participant in the game with unassigned status.
    /// If the game is not locked, it immediately begins the name assignment process.
    ///
    /// # Arguments
    ///
    /// * `watcher` - Unique ID for the new participant
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the participant was added successfully
    /// * `Err(watcher::Error)` if the ID is already in use or invalid
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn add_unassigned<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher: Id,
        tunnel_finder: F,
    ) -> Result<(), watcher::Error> {
        self.watchers.add_watcher(watcher, Value::Unassigned)?;

        if !self.locked {
            self.handle_unassigned(watcher, tunnel_finder);
        }

        Ok(())
    }

    /// Handles incoming messages from participants
    ///
    /// This method processes all incoming messages from participants, validates
    /// that messages are appropriate for the sender's role, and routes them to
    /// the correct handlers based on the current game state.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant sending the message
    /// * `message` - The incoming message to process
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    pub fn receive_message<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(AlarmMessage, web_time::Duration),
    >(
        &mut self,
        watcher_id: Id,
        message: IncomingMessage,
        mut schedule_message: S,
        tunnel_finder: F,
    ) {
        let Some(watcher_value) = self.watchers.get_watcher_value(watcher_id) else {
            return;
        };

        if !message.follows(watcher_value.kind()) {
            return;
        }

        match message {
            IncomingMessage::Unassigned(_) if self.locked => {}
            IncomingMessage::Host(IncomingHostMessage::Lock(lock_state)) => {
                self.locked = lock_state;
            }
            IncomingMessage::Unassigned(IncomingUnassignedMessage::NameRequest(s))
                if self.options.random_names.is_none() =>
            {
                if let Err(e) = self.assign_player_name(watcher_id, &s, &tunnel_finder) {
                    self.watchers.send_message(
                        &UpdateMessage::NameError(e).into(),
                        watcher_id,
                        tunnel_finder,
                    );
                }
            }
            IncomingMessage::Player(IncomingPlayerMessage::ChooseTeammates(preferences)) => {
                if let Some(team_manager) = &mut self.team_manager {
                    team_manager.set_preferences(
                        watcher_id,
                        preferences
                            .into_iter()
                            .filter_map(|name| self.names.get_id(&name))
                            .collect_vec(),
                    );
                }
            }
            message => match &mut self.state {
                State::WaitingScreen | State::TeamDisplay => {
                    if let IncomingMessage::Host(IncomingHostMessage::Next) = message {
                        self.play(schedule_message, &tunnel_finder);
                    }
                }
                State::Slide(current_slide) => {
                    if current_slide.state.receive_message(
                        &mut self.leaderboard,
                        &self.watchers,
                        self.team_manager.as_ref(),
                        &mut schedule_message,
                        watcher_id,
                        &tunnel_finder,
                        message,
                        current_slide.index,
                        self.fuiz_config.len(),
                    ) {
                        self.finish_slide(schedule_message, tunnel_finder);
                    }
                }
                State::Leaderboard(index) => {
                    if let IncomingMessage::Host(IncomingHostMessage::Next) = message {
                        let next_index = *index + 1;
                        if let Some(slide) = self.fuiz_config.slides.get(next_index) {
                            let mut state = slide.to_state();

                            state.play(
                                self.team_manager.as_ref(),
                                &self.watchers,
                                schedule_message,
                                &tunnel_finder,
                                next_index,
                                self.fuiz_config.len(),
                            );

                            self.set_state(State::Slide(Box::new(CurrentSlide {
                                index: next_index,
                                state,
                            })));
                        } else {
                            self.announce_summary(&tunnel_finder);
                        }
                    }
                }
                State::Done => {
                    if let IncomingMessage::Host(IncomingHostMessage::Next) = message {
                        self.mark_as_done(tunnel_finder);
                    }
                }
            },
        }
    }

    /// Handles scheduled alarm messages for timed game events
    ///
    /// This method processes alarm messages that were scheduled to trigger
    /// game state transitions at specific times, such as moving from question
    /// display to answer acceptance or from answers to results.
    ///
    /// # Arguments
    ///
    /// * `message` - The alarm message to process
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    pub fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(AlarmMessage, web_time::Duration),
    >(
        &mut self,
        message: AlarmMessage,
        mut schedule_message: S,
        tunnel_finder: F,
    ) {
        match message {
            AlarmMessage::MultipleChoice(
                multiple_choice::AlarmMessage::ProceedFromSlideIntoSlide {
                    index: slide_index,
                    to: _,
                },
            )
            | AlarmMessage::TypeAnswer(type_answer::AlarmMessage::ProceedFromSlideIntoSlide {
                index: slide_index,
                to: _,
            })
            | AlarmMessage::Order(order::AlarmMessage::ProceedFromSlideIntoSlide {
                index: slide_index,
                to: _,
            }) => match &mut self.state {
                State::Slide(current_slide) if current_slide.index == slide_index => {
                    if current_slide.state.receive_alarm(
                        &mut self.leaderboard,
                        &self.watchers,
                        self.team_manager.as_ref(),
                        &mut schedule_message,
                        &tunnel_finder,
                        message,
                        current_slide.index,
                        self.fuiz_config.len(),
                    ) {
                        self.finish_slide(schedule_message, tunnel_finder);
                    }
                }
                _ => (),
            },
        }
    }

    /// Returns the message necessary to synchronize a participant's state
    ///
    /// This method generates the appropriate synchronization message based on
    /// the current game state and the participant's role. It ensures that
    /// newly connected or reconnecting participants receive the correct view
    /// of the current game state.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant to synchronize
    /// * `watcher_kind` - Type of participant (host, player, unassigned)
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Returns
    ///
    /// A SyncMessage containing the current state information appropriate for the participant
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn state_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher_id: Id,
        watcher_kind: ValueKind,
        tunnel_finder: F,
    ) -> super::SyncMessage {
        match &self.state {
            State::WaitingScreen => match &self.team_manager {
                Some(team_manager)
                    if !team_manager.is_random_assignments()
                        && matches!(watcher_kind, ValueKind::Player) =>
                {
                    let pref: HashSet<Id> = team_manager
                        .get_preferences(watcher_id)
                        .unwrap_or_default()
                        .into_iter()
                        .collect();
                    SyncMessage::ChooseTeammates {
                        max_selection: team_manager.optimal_size,
                        available: self
                            .watchers
                            .specific_vec(ValueKind::Player, tunnel_finder)
                            .into_iter()
                            .filter_map(|(id, _, _)| Some((id, self.watchers.get_name(id)?)))
                            .map(|(id, name)| (name, pref.contains(&id)))
                            .collect(),
                    }
                    .into()
                }
                _ => SyncMessage::WaitingScreen(self.waiting_screen_names(tunnel_finder)).into(),
            },
            State::TeamDisplay => match watcher_kind {
                ValueKind::Player => SyncMessage::FindTeam(
                    self.watchers.get_team_name(watcher_id).unwrap_or_default(),
                )
                .into(),
                _ => SyncMessage::TeamDisplay(
                    self.team_manager
                        .as_ref()
                        .and_then(teams::TeamManager::team_names)
                        .unwrap_or_default(),
                )
                .into(),
            },
            State::Leaderboard(index) => match watcher_kind {
                ValueKind::Host | ValueKind::Unassigned => SyncMessage::Leaderboard {
                    index: *index,
                    count: self.fuiz_config.len(),
                    leaderboard: self.leaderboard_message(),
                }
                .into(),
                ValueKind::Player => SyncMessage::Score {
                    index: *index,
                    count: self.fuiz_config.len(),
                    score: self.score(watcher_id),
                }
                .into(),
            },
            State::Slide(current_slide) => current_slide.state.state_message(
                watcher_id,
                watcher_kind,
                self.team_manager.as_ref(),
                &self.watchers,
                tunnel_finder,
                current_slide.index,
                self.fuiz_config.len(),
            ),
            State::Done => match watcher_kind {
                ValueKind::Host => SyncMessage::Summary({
                    let (player_count, stats) =
                        self.leaderboard.host_summary(!self.options.no_leaderboard);
                    SummaryMessage::Host {
                        stats,
                        player_count,
                        config: self.fuiz_config.clone(),
                        options: self.options,
                    }
                })
                .into(),
                ValueKind::Player => SyncMessage::Summary(SummaryMessage::Player {
                    score: if self.options.no_leaderboard {
                        None
                    } else {
                        self.score(watcher_id)
                    },
                    points: self.leaderboard.player_summary(
                        self.leaderboard_id(watcher_id),
                        !self.options.no_leaderboard,
                    ),
                    config: self.fuiz_config.clone(),
                })
                .into(),
                ValueKind::Unassigned => SyncMessage::NotAllowed.into(),
            },
        }
    }

    /// Updates the session associated with a participant (for reconnection)
    ///
    /// This method handles participant reconnection by updating their session
    /// and sending them the current game state. It handles different participant
    /// types appropriately and manages locked game states.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant reconnecting
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn update_session<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher_id: Id,
        tunnel_finder: F,
    ) {
        let Some(watcher_value) = self.watchers.get_watcher_value(watcher_id) else {
            return;
        };

        match watcher_value.clone() {
            Value::Host => {
                self.watchers.send_state(
                    &self.state_message(watcher_id, watcher_value.kind(), &tunnel_finder),
                    watcher_id,
                    &tunnel_finder,
                );
                self.watchers.send_state(
                    &SyncMessage::Metainfo(MetainfoMessage::Host {
                        locked: self.locked,
                    })
                    .into(),
                    watcher_id,
                    tunnel_finder,
                );
            }
            Value::Player(player_value) => {
                if let PlayerValue::Team {
                    team_name,
                    individual_name: _,
                    team_id: _,
                } = &player_value
                {
                    self.watchers.send_message(
                        &UpdateMessage::FindTeam(team_name.clone()).into(),
                        watcher_id,
                        &tunnel_finder,
                    );
                }
                self.watchers.send_message(
                    &UpdateMessage::NameAssign(player_value.name().to_owned()).into(),
                    watcher_id,
                    &tunnel_finder,
                );
                self.update_player_with_options(watcher_id, &tunnel_finder);
                self.watchers.send_state(
                    &self.state_message(watcher_id, watcher_value.kind(), &tunnel_finder),
                    watcher_id,
                    &tunnel_finder,
                );
            }
            Value::Unassigned if self.locked => {}
            Value::Unassigned => {
                self.handle_unassigned(watcher_id, &tunnel_finder);
            }
        }
    }
}
