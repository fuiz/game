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

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct PreferenceGroup(usize, Vec<Id>);

impl From<Vec<Id>> for PreferenceGroup {
    fn from(players: Vec<Id>) -> Self {
        Self(players.len(), players)
    }
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
        if self.teams.get().is_none() {
            let players = self.get_players(watchers, tunnel_finder);
            let preference_groups = self.create_preference_groups(&players);
            let balanced_teams = self.balance_teams(preference_groups, players.len());
            let team_id_names = self.create_team_id_names(balanced_teams, names);
            let result = self.assign_all_players_to_teams(&team_id_names, names, watchers);
            let _ = self.teams.set(result);
        }
    }

    fn get_players<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watchers: &Watchers,
        tunnel_finder: F,
    ) -> Vec<Id> {
        watchers
            .specific_vec(watcher::ValueKind::Player, tunnel_finder)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect_vec()
    }

    fn get_player_preferences(&self, player_id: Id) -> Option<Vec<Id>> {
        self.preferences
            .as_ref()
            .and_then(|p| p.get(&player_id))
            .map(|p| p.to_owned())
    }

    fn create_preference_groups(&self, players: &[Id]) -> Vec<Vec<Id>> {
        let mut preference_groups = players
            .iter()
            .map(|&id| {
                let mutual_preferences = self
                    .get_player_preferences(id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|&pref| {
                        self.get_player_preferences(pref)
                            .unwrap_or_default()
                            .contains(&id)
                    })
                    .min()
                    .unwrap_or(id)
                    .min(id);
                (mutual_preferences, id)
            })
            .sorted()
            .chunk_by(|(smallest_mutual, _)| *smallest_mutual)
            .into_iter()
            .map(|(_, group)| {
                let mut players: Vec<Id> = group.map(|(_, player_id)| player_id).collect();
                fastrand::shuffle(&mut players);
                players
            })
            .sorted_by_key(|group| group.len())
            .rev()
            .collect_vec();

        if preference_groups.is_empty() {
            preference_groups.push(Vec::new());
        }

        preference_groups
    }

    fn balance_teams(&self, teams: Vec<Vec<Id>>, players_count: usize) -> Vec<Vec<Id>> {
        if teams.len() == players_count {
            self.redistribute_single_player_teams(teams)
        } else {
            self.merge_teams_optimally(teams)
        }
    }

    fn redistribute_single_player_teams(&self, teams: Vec<Vec<Id>>) -> Vec<Vec<Id>> {
        let total_teams = teams.len().div_ceil(self.optimal_size);
        let remainder = teams.len() % total_teams;
        let base_size = teams.len() / total_teams;

        let small_team_count = total_teams - remainder;
        let big_team_size = base_size + 1;

        let split_point = small_team_count * base_size;
        let (small_teams, big_teams) = teams.split_at(split_point);

        small_teams
            .chunks(base_size)
            .map(|chunk| chunk.iter().flatten().copied().collect())
            .chain(
                big_teams
                    .chunks(big_team_size)
                    .map(|chunk| chunk.iter().flatten().copied().collect()),
            )
            .collect()
    }

    fn merge_teams_optimally(&self, teams: Vec<Vec<Id>>) -> Vec<Vec<Id>> {
        let mut sorted_teams: BTreeSet<PreferenceGroup> = BTreeSet::new();

        for team in teams {
            let available_space = self.optimal_size.saturating_sub(team.len()) + 1;

            if let Some(compatible_team) = sorted_teams
                .range(..(PreferenceGroup(available_space, Vec::new())))
                .next_back()
                .map(|group| group.1.clone())
            {
                sorted_teams.remove(&compatible_team.clone().into());
                let merged_team: Vec<Id> = team.into_iter().chain(compatible_team).collect();
                sorted_teams.insert(merged_team.into());
            } else {
                sorted_teams.insert(team.into());
            }
        }

        self.consolidate_single_member_teams(sorted_teams)
    }

    fn consolidate_single_member_teams(
        &self,
        mut teams: BTreeSet<PreferenceGroup>,
    ) -> Vec<Vec<Id>> {
        if let Some(smallest) = teams.pop_first() {
            if smallest.0 == 1 {
                if let Some(second_smallest) = teams.pop_first() {
                    let consolidated: Vec<Id> =
                        smallest.1.into_iter().chain(second_smallest.1).collect();
                    teams.insert(consolidated.into());
                } else {
                    teams.insert(smallest);
                }
            } else {
                teams.insert(smallest);
            }
        }

        teams.into_iter().map(|group| group.1).collect()
    }

    fn create_team_id_names(
        &self,
        teams: Vec<Vec<Id>>,
        names: &mut names::Names,
    ) -> Vec<(Id, String, Vec<Id>)> {
        teams
            .into_iter()
            .map(|players| {
                let team_id = Id::new();
                let team_name = self.generate_unique_team_name(team_id, names);
                (team_id, team_name, players)
            })
            .collect()
    }

    fn assign_all_players_to_teams(
        &mut self,
        teams: &[(Id, String, Vec<Id>)],
        names: &names::Names,
        watchers: &mut Watchers,
    ) -> Vec<(Id, String)> {
        teams
            .iter()
            .map(|(team_id, team_name, players)| {
                self.assign_players_to_team(players, *team_id, team_name, names, watchers);
                (*team_id, team_name.clone())
            })
            .collect()
    }

    fn generate_unique_team_name(&self, team_id: Id, names: &mut names::Names) -> String {
        loop {
            let name = self.name_style.get_name();
            let plural_name = pluralizer::pluralize(&name, 2, false);

            if let Ok(unique_name) = names.set_name(team_id, &plural_name) {
                return unique_name;
            }
        }
    }

    fn assign_players_to_team(
        &mut self,
        players: &[Id],
        team_id: Id,
        team_name: &str,
        names: &names::Names,
        watchers: &mut Watchers,
    ) {
        for &player_id in players {
            self.player_to_team.insert(player_id, team_id);
            watchers.update_watcher_value(
                player_id,
                watcher::Value::Player(watcher::PlayerValue::Team {
                    team_name: team_name.to_owned(),
                    individual_name: names.get_name(&player_id).unwrap_or_default(),
                    team_id,
                }),
            );
        }
        self.team_to_players.insert(team_id, players.to_vec());
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

    #[test]
    fn test_random_assignment() {
        let manager = TeamManager::new(3, true, NameStyle::default());
        assert!(manager.is_random_assignments());
        assert!(manager.preferences.is_none());
    }

    #[test]
    fn test_non_random_assignment() {
        let manager = TeamManager::new(3, false, NameStyle::default());
        assert!(!manager.is_random_assignments());
        assert!(manager.preferences.is_some());
    }

    #[test]
    fn test_set_preferences() {
        let mut manager = TeamManager::new(3, false, NameStyle::default());
        let player1 = Id::new();
        let player2 = Id::new();
        let preferences = vec![player2];

        manager.set_preferences(player1, preferences.clone());
        assert_eq!(manager.get_preferences(player1), Some(preferences));
    }

    #[test]
    fn test_set_preferences_random_mode() {
        let mut manager = TeamManager::new(3, true, NameStyle::default());
        let player1 = Id::new();
        let player2 = Id::new();
        let preferences = vec![player2];

        manager.set_preferences(player1, preferences);
        assert_eq!(manager.get_preferences(player1), None);
    }

    #[test]
    fn test_team_names() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        assert!(manager.team_names().is_none());

        let player1 = Id::new();
        let player2 = Id::new();

        for player in [player1, player2] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_names = manager.team_names().unwrap();
        assert_eq!(team_names.exact_count(), 1);
        assert!(team_names.items().len() > 0);
    }

    #[test]
    fn test_add_player_after_finalization() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();

        for player in [player1, player2] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        watchers
            .add_watcher(
                player3,
                watcher::Value::Player(watcher::PlayerValue::Individual {
                    name: format!("Player {player3}"),
                }),
            )
            .unwrap();

        let team_name = manager.add_player(player3, &mut watchers);
        assert!(team_name.is_some());
        assert!(manager.get_team(player3).is_some());
    }

    #[test]
    fn test_add_player_already_assigned() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();

        watchers
            .add_watcher(
                player1,
                watcher::Value::Player(watcher::PlayerValue::Individual {
                    name: format!("Player {player1}"),
                }),
            )
            .unwrap();

        manager.finalize(&mut watchers, &mut names, tunnel);

        let first_assignment = manager.add_player(player1, &mut watchers);
        let second_assignment = manager.add_player(player1, &mut watchers);

        assert_eq!(first_assignment, second_assignment);
    }

    #[test]
    fn test_team_size() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();

        for player in [player1, player2] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        assert_eq!(manager._team_size(player1), Some(2));
        assert_eq!(manager._team_size(player2), Some(2));

        let unassigned_player = Id::new();
        assert_eq!(manager._team_size(unassigned_player), None);
    }

    #[test]
    fn test_team_index() {
        let mut manager = TeamManager::new(3, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();

        for player in [player1, player2, player3] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        let index = manager.team_index(player1, |_| true);
        assert!(index.is_some());
        assert!(index.unwrap() < 3);

        let no_match_index = manager.team_index(player1, |_| false);
        assert_eq!(no_match_index, None);

        let unassigned_player = Id::new();
        assert_eq!(manager.team_index(unassigned_player, |_| true), None);
    }

    #[test]
    fn test_all_ids() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        assert_eq!(manager.all_ids(), Vec::<Id>::new());

        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();

        for player in [player1, player2] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_ids = manager.all_ids();
        assert_eq!(team_ids.len(), 1);
    }

    #[test]
    fn test_preferences_with_mutual_preferences() {
        let mut manager = TeamManager::new(4, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();
        let player4 = Id::new();

        for player in [player1, player2, player3, player4] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.set_preferences(player1, vec![player2]);
        manager.set_preferences(player2, vec![player1]);

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team1 = manager.get_team(player1);
        let team2 = manager.get_team(player2);
        assert_eq!(team1, team2);
    }

    #[test]
    fn test_complex_team_formation_with_preferences() {
        let mut manager = TeamManager::new(3, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();
        let player4 = Id::new();
        let player5 = Id::new();

        for player in [player1, player2, player3, player4, player5] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.set_preferences(player1, vec![player2]);
        manager.set_preferences(player2, vec![player3]);
        manager.set_preferences(player3, vec![player1]);
        manager.set_preferences(player4, vec![player5]);

        manager.finalize(&mut watchers, &mut names, tunnel);

        assert!(manager.get_team(player1).is_some());
        assert!(manager.get_team(player2).is_some());
        assert!(manager.get_team(player3).is_some());
        assert!(manager.get_team(player4).is_some());
        assert!(manager.get_team(player5).is_some());
    }

    #[test]
    fn test_single_player_teams_consolidation() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();

        for player in [player1, player2, player3] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_ids = manager.all_ids();
        assert!(team_ids.len() >= 1);

        let team_sizes: Vec<usize> = team_ids
            .iter()
            .map(|&team_id| manager.team_to_players.get(&team_id).unwrap().len())
            .collect();

        assert!(team_sizes.iter().all(|&size| size >= 1));
    }

    #[test]
    fn test_empty_teams_edge_case() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_ids = manager.all_ids();
        assert_eq!(team_ids.len(), 1);
    }

    #[test]
    fn test_add_player_before_finalization() {
        let mut manager = TeamManager::new(2, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let player = Id::new();

        watchers
            .add_watcher(
                player,
                watcher::Value::Player(watcher::PlayerValue::Individual {
                    name: format!("Player {player}"),
                }),
            )
            .unwrap();

        let result = manager.add_player(player, &mut watchers);
        assert_eq!(result, None);
    }

    #[test]
    fn test_single_team_consolidation() {
        let mut manager = TeamManager::new(3, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();
        let player4 = Id::new();

        for player in [player1, player2, player3, player4] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.set_preferences(player1, vec![player2]);
        manager.set_preferences(player3, vec![player4]);
        manager.set_preferences(player4, vec![player3]);

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_ids = manager.all_ids();
        assert!(team_ids.len() >= 1);

        let team_sizes: Vec<usize> = team_ids
            .iter()
            .map(|&team_id| manager.team_to_players.get(&team_id).unwrap().len())
            .collect();

        assert!(team_sizes.iter().all(|&size| size >= 1));
    }

    #[test]
    fn test_single_member_team_consolidation() {
        let mut manager = TeamManager::new(4, false, NameStyle::default());
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);
        let mut names = names::Names::default();
        let tunnel = |_id| Some(MockTunnel {});

        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();

        for player in [player1, player2, player3] {
            watchers
                .add_watcher(
                    player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {player}"),
                    }),
                )
                .unwrap();
        }

        manager.set_preferences(player2, vec![player3]);
        manager.set_preferences(player3, vec![player2]);

        manager.finalize(&mut watchers, &mut names, tunnel);

        let team_ids = manager.all_ids();
        assert!(team_ids.len() >= 1);

        let team1 = manager.get_team(player1);
        let team2 = manager.get_team(player2);
        let team3 = manager.get_team(player3);

        assert!(team1.is_some());
        assert!(team2.is_some());
        assert!(team3.is_some());

        assert_eq!(team2, team3);

        let team_sizes: Vec<usize> = team_ids
            .iter()
            .map(|&team_id| manager.team_to_players.get(&team_id).unwrap().len())
            .collect();

        assert!(team_sizes.iter().all(|&size| size >= 1));
    }
}
