use std::collections::{BTreeSet, HashMap};

use itertools::Itertools;
use once_cell_serde::sync::OnceCell;
use serde::{Deserialize, Serialize};

use crate::game::NameStyle;

use super::{
    names,
    session::Tunnel,
    watcher::{self, Id, Watchers},
    TruncatedVec,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct TeamManager {
    player_to_team: HashMap<Id, Id>,
    pub optimal_size: usize,
    assign_random: bool,
    name_style: NameStyle,

    preferences: Option<HashMap<Id, Vec<Id>>>,

    teams: OnceCell<Vec<(Id, String)>>,
    next_team_to_receive_player: usize,

    team_to_players: HashMap<Id, Vec<Id>>,
}

impl TeamManager {
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

    pub fn is_random_assignments(&self) -> bool {
        self.assign_random
    }

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
                    .into_iter()
                    .chunks(size_of_small_teams)
                    .into_iter()
                    .map(|chunk| chunk.into_iter().flatten().copied().collect_vec())
                    .chain(
                        big_teams
                            .into_iter()
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

            let final_teams = existing_teams
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
                .collect_vec();

            final_teams
        });
    }

    pub fn team_names(&self) -> Option<TruncatedVec<String>> {
        self.teams.get().map(|v| {
            TruncatedVec::new(
                v.iter().map(|(_, team_name)| team_name.to_owned()),
                50,
                v.len(),
            )
        })
    }

    pub fn get_team(&self, player_id: Id) -> Option<Id> {
        self.player_to_team.get(&player_id).copied()
    }

    pub fn set_preferences(&mut self, player_id: Id, preferences: Vec<Id>) {
        if let Some(prefs) = &mut self.preferences {
            prefs.insert(player_id, preferences);
        }
    }

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

    pub fn _team_size(&self, player_id: Id) -> Option<usize> {
        self.get_team(player_id)
            .and_then(|team_id| self.team_to_players.get(&team_id))
            .map(|p| p.len())
    }

    pub fn team_members(&self, player_id: Id) -> Option<Vec<Id>> {
        self.get_team(player_id)
            .and_then(|team_id| self.team_to_players.get(&team_id).cloned())
    }

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

    pub fn all_ids(&self) -> Vec<Id> {
        self.teams.get().map_or(Vec::new(), |teams| {
            teams.iter().map(|(id, _)| *id).collect_vec()
        })
    }

    pub fn get_preferences(&self, watcher_id: Id) -> Option<Vec<Id>> {
        self.preferences
            .as_ref()
            .and_then(|p| p.get(&watcher_id))
            .map(|p| p.to_owned())
    }
}

#[cfg(test)]
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
            assert!(watchers
                .add_watcher(
                    *player,
                    watcher::Value::Player(watcher::PlayerValue::Individual {
                        name: format!("Player {}", player)
                    }),
                )
                .is_ok());
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
                "Player at index {} should have {} team members",
                i,
                expected_size
            );
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_2() {
        for i in 1..=10 {
            test_team_distribution(2 * i, 2, vec![2].repeat(i));
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_3() {
        for i in 1..=10 {
            test_team_distribution(3 * i, 3, vec![3].repeat(i));
        }
    }

    #[test]
    fn test_teams_perfect_distribution_team_size_4() {
        for i in 1..=10 {
            test_team_distribution(4 * i, 4, vec![4].repeat(i));
        }
    }

    #[test]
    fn test_teams_additional_person_team_size_2() {
        for i in 1..=10 {
            let mut team_sizes = vec![2].repeat(i);
            team_sizes.insert(0, 1);
            test_team_distribution(2 * i + 1, 2, team_sizes);
        }
    }

    #[test]
    fn test_teams_additional_person_team_size_3() {
        for i in 1..=10 {
            let mut team_sizes = vec![3].repeat(i - 1);
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
            let mut team_sizes = vec![4].repeat(i - 2);
            team_sizes.insert(0, 3);
            team_sizes.insert(0, 3);
            team_sizes.insert(0, 3);
            test_team_distribution(4 * i + 1, 4, team_sizes);
        }
    }
}
