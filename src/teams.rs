//! Team formation and management
//!
//! This module handles the formation and management of teams in team-based
//! Fuiz games. It supports both random team assignment and preference-based
//! team formation where players can choose their preferred teammates.

use std::collections::{BTreeSet, HashMap};

use itertools::Itertools;
use once_cell_serde::sync::OnceCell;
use serde::{Deserialize, Serialize};

use crate::game::NameStyle;

use super::{
    TruncatedVec, names,
    session::Tunnel,
    watcher::{self, Id, Watchers},
};

/// Manages team formation and player-to-team assignments
///
/// This struct handles the complex process of forming balanced teams,
/// either through random assignment or by respecting player preferences
/// for teammates. It also manages team naming and maintains the mapping
/// between players and their assigned teams.
#[derive(Debug, Serialize, Deserialize)]
pub struct TeamManager {
    /// Mapping from player ID to their team ID
    player_to_team: HashMap<Id, Id>,
    /// Ideal size for each team
    pub optimal_size: usize,
    /// Whether to use random assignment or preference-based assignment
    assign_random: bool,
    /// Style for generating team names
    name_style: NameStyle,

    /// Player preferences for teammates (only used in non-random mode)
    preferences: Option<HashMap<Id, Vec<Id>>>,

    /// Finalized list of teams with their IDs and names (computed once)
    teams: OnceCell<Vec<(Id, String)>>,
    /// Index for round-robin assignment of players to teams
    next_team_to_receive_player: usize,

    /// Mapping from team ID to list of player IDs in that team
    team_to_players: HashMap<Id, Vec<Id>>,
}

impl TeamManager {
    /// Creates a new team manager with the specified configuration
    ///
    /// # Arguments
    ///
    /// * `optimal_size` - The ideal number of players per team
    /// * `assign_random` - Whether to assign players randomly or use preferences
    /// * `name_style` - The style for generating team names
    ///
    /// # Returns
    ///
    /// A new TeamManager instance ready for team formation
    pub fn new(optimal_size: usize, assign_random: bool, name_style: NameStyle) -> Self {
        Self {
            player_to_team: HashMap::default(),
            team_to_players: HashMap::default(),
            assign_random,
            name_style,
            optimal_size,
            preferences: if assign_random {
                None
            } else {
                Some(HashMap::default())
            },
            teams: OnceCell::default(),
            next_team_to_receive_player: 0,
        }
    }

    /// Returns whether this team manager uses random assignment
    ///
    /// # Returns
    ///
    /// `true` if teams are formed randomly, `false` if preferences are used
    pub fn is_random_assignments(&self) -> bool {
        self.assign_random
    }

    /// Finalizes team formation and assigns all players to teams
    ///
    /// This method performs the actual team formation process, creating
    /// teams based on player preferences (if enabled) or random assignment.
    /// It also generates team names and updates player objects with their
    /// team information.
    ///
    /// # Arguments
    ///
    /// * `watchers` - The watchers manager containing all players
    /// * `names` - The names manager for generating team names
    /// * `tunnel_finder` - Function to find communication tunnels for players
    pub fn finalize<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watchers: &mut Watchers,
        names: &mut names::Names,
        tunnel_finder: F,
    ) {
        let optimal_size = self.optimal_size;
        let preferences = &self.preferences;
        let player_to_team = &mut self.player_to_team;
        let team_to_players = &mut self.team_to_players;

        let get_preferences = |player_id: Id| -> Option<Vec<Id>> {
            preferences
                .as_ref()
                .and_then(|p| p.get(&player_id))
                .map(|p| p.to_owned())
        };

        self.teams.get_or_init(|| {
            let players = watchers
                .specific_vec(watcher::ValueKind::Player, tunnel_finder)
                .into_iter()
                .map(|(id, _, _)| id)
                .collect_vec();

            let players_count = players.len();

            let mut existing_teams = players
                .into_iter()
                .map(|id| {
                    (
                        get_preferences(id)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|pref| {
                                get_preferences(*pref)
                                    .unwrap_or_default()
                                    .into_iter()
                                    .any(|prefs_pref| prefs_pref == id)
                            })
                            .min()
                            .unwrap_or(id)
                            .min(id),
                        id,
                    )
                })
                .sorted()
                .chunk_by(|(smallest_moot, _)| *smallest_moot)
                .into_iter()
                .map(|(_, g)| {
                    // to guard against attacks
                    let mut players = g.map(|(_, player_id)| player_id).collect_vec();
                    fastrand::shuffle(&mut players);
                    players
                })
                .sorted_by_key(std::vec::Vec::len)
                .rev()
                .collect_vec();

            if existing_teams.is_empty() {
                existing_teams.push(Vec::new());
            }

            if existing_teams.len() == players_count {
                let total_teams = existing_teams.len().div_ceil(optimal_size);

                let how_many_big_teams = existing_teams.len() % total_teams;
                let how_many_small_teams = total_teams - how_many_big_teams;

                let size_of_small_teams = existing_teams.len() / total_teams;
                let size_of_big_teams = size_of_small_teams + 1;

                let (small_teams, big_teams) =
                    existing_teams.split_at(how_many_small_teams * size_of_small_teams);

                existing_teams = small_teams
                    .iter()
                    .chunks(size_of_small_teams)
                    .into_iter()
                    .map(|chunk| chunk.into_iter().flatten().copied().collect_vec())
                    .chain(
                        big_teams
                            .iter()
                            .chunks(size_of_big_teams)
                            .into_iter()
                            .map(|chunk| chunk.into_iter().flatten().copied().collect_vec()),
                    )
                    .collect_vec();
            } else {
                #[derive(PartialEq, Eq, PartialOrd, Ord)]
                struct PreferenceGroup(usize, Vec<Id>);

                impl From<Vec<Id>> for PreferenceGroup {
                    fn from(value: Vec<Id>) -> Self {
                        Self(value.len(), value)
                    }
                }

                let mut tree: BTreeSet<PreferenceGroup> = BTreeSet::new();

                for prefs in existing_teams {
                    if let Some(bucket) = tree
                        .range(..(PreferenceGroup(optimal_size - prefs.len() + 1, Vec::new())))
                        .next_back()
                        .map(|b| b.1.clone())
                    {
                        tree.remove(&bucket.clone().into());
                        tree.insert(prefs.into_iter().chain(bucket).collect_vec().into());
                    } else {
                        tree.insert(prefs.into());
                    }
                }

                if tree.len() >= 2 {
                    if let Some(first) = tree.first() {
                        if first.0 == 1 {
                            if let (Some(smallest), Some(second_smallest)) =
                                (tree.pop_first(), tree.pop_first())
                            {
                                tree.insert(
                                    smallest
                                        .1
                                        .into_iter()
                                        .chain(second_smallest.1)
                                        .collect_vec()
                                        .into(),
                                );
                            }
                        }
                    }
                }

                existing_teams = tree.into_iter().map(|p| p.1).collect_vec();
            }

            existing_teams
                .into_iter()
                .map(|players| {
                    let team_id = Id::new();

                    let team_name = loop {
                        let Some(name) = self.name_style.get_name() else {
                            continue;
                        };

                        let plural_name = pluralizer::pluralize(&name, 2, false);

                        if let Ok(unique_name) = names.set_name(team_id, &plural_name) {
                            break unique_name;
                        }
                    };

                    players.iter().copied().for_each(|player_id| {
                        player_to_team.insert(player_id, team_id);
                        watchers.update_watcher_value(
                            player_id,
                            watcher::Value::Player(watcher::PlayerValue::Team {
                                team_name: team_name.clone(),
                                individual_name: names.get_name(&player_id).unwrap_or_default(),
                                team_id,
                            }),
                        );
                    });

                    team_to_players.insert(team_id, players.to_vec());

                    (team_id, team_name)
                })
                .collect_vec()
        });
    }

    /// Gets the names of all formed teams
    ///
    /// Returns a truncated list of team names that have been created during
    /// the team formation process. This is used for displaying team information
    /// to participants.
    ///
    /// # Returns
    ///
    /// `Some(TruncatedVec<String>)` containing team names if teams have been
    /// finalized, or `None` if team formation hasn't completed yet
    pub fn team_names(&self) -> Option<TruncatedVec<String>> {
        self.teams.get().map(|v| {
            TruncatedVec::new(
                v.iter().map(|(_, team_name)| team_name.to_owned()),
                50,
                v.len(),
            )
        })
    }

    /// Gets the team ID for a specific player
    ///
    /// Looks up which team a player has been assigned to during the team
    /// formation process.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player's unique identifier
    ///
    /// # Returns
    ///
    /// `Some(Id)` containing the team's ID if the player is assigned to a team,
    /// or `None` if the player hasn't been assigned yet
    pub fn get_team(&self, player_id: Id) -> Option<Id> {
        self.player_to_team.get(&player_id).copied()
    }

    /// Sets teammate preferences for a player
    ///
    /// Records a player's preferred teammates for use during team formation.
    /// This is only relevant when random assignment is disabled.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player setting preferences
    /// * `preferences` - List of preferred teammate IDs
    pub fn set_preferences(&mut self, player_id: Id, preferences: Vec<Id>) {
        if let Some(prefs) = &mut self.preferences {
            prefs.insert(player_id, preferences);
        }
    }

    /// Adds a new player to an existing team
    ///
    /// Assigns a player to a team after the initial team formation has been
    /// completed. Uses round-robin assignment to balance team sizes.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player to add to a team
    /// * `watchers` - The watchers manager to update with team information
    ///
    /// # Returns
    ///
    /// `Some(String)` containing the team name if successfully added,
    /// or `None` if team formation hasn't been completed
    pub fn add_player(&mut self, player_id: Id, watchers: &mut Watchers) -> Option<String> {
        if let Some(team) = self.get_team(player_id) {
            return self
                .teams
                .get()
                .and_then(|teams| teams.iter().find(|(id, _)| *id == team))
                .map(|(_, name)| name.to_owned());
        }

        if let Some(teams) = self.teams.get() {
            let next_index = self.next_team_to_receive_player;

            self.next_team_to_receive_player += 1;

            let (team_id, team_name) = teams
                .get(next_index % teams.len())
                .expect("there is always at least one team");

            self.player_to_team.insert(player_id, *team_id);

            let p = self
                .team_to_players
                .get_mut(team_id)
                .expect("team should exist");

            p.push(player_id);

            watchers.update_watcher_value(
                player_id,
                watcher::Value::Player(watcher::PlayerValue::Team {
                    team_name: team_name.to_owned(),
                    individual_name: watchers.get_name(player_id).unwrap_or_default(),
                    team_id: *team_id,
                }),
            );

            Some(team_name.to_owned())
        } else {
            None
        }
    }

    /// Gets the size of a player's team
    ///
    /// Returns the current number of players in the team that the specified
    /// player belongs to.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player whose team size to check
    ///
    /// # Returns
    ///
    /// `Some(usize)` containing the team size, or `None` if the player
    /// is not assigned to a team
    pub fn _team_size(&self, player_id: Id) -> Option<usize> {
        self.get_team(player_id)
            .and_then(|team_id| self.team_to_players.get(&team_id))
            .map(|p| p.len())
    }

    /// Gets all members of a player's team
    ///
    /// Returns the list of all player IDs that belong to the same team
    /// as the specified player.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player whose teammates to retrieve
    ///
    /// # Returns
    ///
    /// `Some(Vec<Id>)` containing all team member IDs (including the player),
    /// or `None` if the player is not assigned to a team
    pub fn team_members(&self, player_id: Id) -> Option<Vec<Id>> {
        self.get_team(player_id)
            .and_then(|team_id| self.team_to_players.get(&team_id).cloned())
    }

    /// Gets a player's index within their team
    ///
    /// Determines the positional index of a player within their team,
    /// considering only team members that satisfy the provided condition.
    /// This is useful for determining speaking order or turn-based interactions.
    ///
    /// # Arguments
    ///
    /// * `player_id` - The player whose team index to find
    /// * `f` - Filter function to determine which team members to consider
    ///
    /// # Returns
    ///
    /// `Some(usize)` containing the player's index within their filtered team,
    /// or `None` if the player is not found in the team or not assigned to a team
    ///
    /// # Type Parameters
    ///
    /// * `F` - Function type for filtering team members
    pub fn team_index<F: Fn(Id) -> bool>(&self, player_id: Id, f: F) -> Option<usize> {
        self.get_team(player_id)
            .and_then(|team_id| self.team_to_players.get(&team_id))
            .and_then(|p| {
                p.iter()
                    .filter(|id| f(**id))
                    .enumerate()
                    .find_map(|(index, current_player_id)| {
                        if *current_player_id == player_id {
                            Some(index)
                        } else {
                            None
                        }
                    })
            })
    }

    /// Gets all team IDs that have been created
    ///
    /// Returns a list of all team identifiers that were created during
    /// the team formation process.
    ///
    /// # Returns
    ///
    /// A vector containing all team IDs, or an empty vector if teams
    /// haven't been finalized yet
    pub fn all_ids(&self) -> Vec<Id> {
        self.teams.get().map_or(Vec::new(), |teams| {
            teams.iter().map(|(id, _)| *id).collect_vec()
        })
    }

    /// Gets the teammate preferences for a specific player
    ///
    /// Retrieves the list of preferred teammates that a player specified
    /// during team formation (only relevant for non-random team assignment).
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The player whose preferences to retrieve
    ///
    /// # Returns
    ///
    /// `Some(Vec<Id>)` containing the player's preferred teammate IDs,
    /// or `None` if the player hasn't set preferences or preferences aren't used
    pub fn get_preferences(&self, watcher_id: Id) -> Option<Vec<Id>> {
        self.preferences
            .as_ref()
            .and_then(|p| p.get(&watcher_id))
            .map(|p| p.to_owned())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    struct MockTunnel {}

    impl Tunnel for MockTunnel {
        fn send_message(&self, _message: &crate::UpdateMessage) {}

        fn send_state(&self, _state: &crate::SyncMessage) {}

        fn close(self) {}
    }

    /// Helper function to test team distribution with a given number of players and team size
    fn test_team_distribution(num_players: usize, optimal_size: usize, team_sizes: Vec<usize>) {
        let mut manager = TeamManager::new(optimal_size, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        // Create the specified number of players
        let mut players: Vec<Id> = (0..num_players).map(|_| Id::new()).collect();

        // Add all players to watchers
        for player in &players {
            assert!(
                watchers
                    .add_watcher(
                        *player,
                        watcher::Value::Player(watcher::PlayerValue::Individual {
                            name: format!("Player {player}")
                        }),
                    )
                    .is_ok()
            );
        }

        // Add all players to manager
        for player in &players {
            assert_eq!(manager.add_player(*player, &mut watchers), None);
        }

        // Finalize team assignment
        manager.finalize(&mut watchers, &mut names, tunnel);

        // Sort players (to match original test's behavior)
        players.sort();
        players.reverse();

        let prefix_sum: Vec<usize> = team_sizes
            .iter()
            .scan(0, |acc, &x| {
                *acc += x;
                Some(*acc)
            })
            .collect();

        dbg!(&team_sizes);
        dbg!(&prefix_sum);

        // Check that teams are correctly sized
        for (i, player) in players.iter().enumerate() {
            let team_members = manager.team_members(*player).unwrap();

            let expected_size = team_sizes[prefix_sum.iter().position(|&x| x > i).unwrap()];

            assert_eq!(
                team_members.len(),
                expected_size,
                "Player at index {i} should have {expected_size} team members"
            );
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_2() {
        for i in 1..=10 {
            test_team_distribution(2 * i, 2, [2].repeat(i));
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_3() {
        for i in 1..=10 {
            test_team_distribution(3 * i, 3, [3].repeat(i));
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_4() {
        for i in 1..=10 {
            test_team_distribution(4 * i, 4, [4].repeat(i));
        }
    }

    #[test]
    fn test_teams_additional_person_team_size_2() {
        for i in 1..=10 {
            let mut team_sizes = [2].repeat(i);
            team_sizes.insert(0, 1);
            test_team_distribution(2 * i + 1, 2, team_sizes);
        }
    }

    #[test]
    fn test_teams_additional_person_team_size_3() {
        for i in 1..=10 {
            let mut team_sizes = [3].repeat(i - 1);
            team_sizes.insert(0, 2);
            team_sizes.insert(0, 2);
            test_team_distribution(3 * i + 1, 3, team_sizes);
        }
    }

    #[test]
    fn test_teams_additional_person_team_size_4() {
        test_team_distribution(5, 4, vec![2, 3]);
        test_team_distribution(6, 4, vec![3, 3]);
        test_team_distribution(7, 4, vec![3, 4]);
        test_team_distribution(9, 4, vec![3, 3, 3]);
        test_team_distribution(10, 4, vec![3, 3, 4]);
        test_team_distribution(11, 4, vec![3, 4, 4]);

        for i in 3..=10 {
            let mut team_sizes = [4].repeat(i - 2);
            team_sizes.insert(0, 3);
            team_sizes.insert(0, 3);
            team_sizes.insert(0, 3);
            test_team_distribution(4 * i + 1, 4, team_sizes);
        }
    }
}
