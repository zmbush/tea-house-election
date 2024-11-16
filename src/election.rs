use poise::serenity_prelude as serenity;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Name(String);

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<S: Into<String>> From<S> for Name {
    fn from(value: S) -> Self {
        Name(value.into())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region(String);

impl std::fmt::Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<S: Into<String>> From<S> for Region {
    fn from(value: S) -> Self {
        Region(value.into())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ballot {
    pub votes: BTreeMap<Name, usize>,
}

impl Ballot {
    pub fn make_embed(&self) -> serenity::CreateEmbed {
        let embed = serenity::CreateEmbed::new()
            .title("Your current ballot")
            .color(serenity::Color::DARK_GREEN)
            .field(
                "Votes",
                self.votes
                    .iter()
                    .map(|(n, r)| format!("* {n} {r}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        embed
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Election {
    owner: serenity::UserId,
    pub candidates: BTreeMap<Name, Region>,
    offices: usize,
    reserved_offices: Vec<Region>,
    pub ballots: BTreeMap<serenity::UserId, Ballot>,
}

impl Election {
    pub fn new<UID: Into<serenity::UserId>>(owner: UID, offices: usize) -> Election {
        Election {
            owner: owner.into(),
            offices,

            candidates: BTreeMap::new(),
            reserved_offices: Vec::new(),
            ballots: BTreeMap::new(),
        }
    }

    pub fn owner(&self) -> &serenity::UserId {
        &self.owner
    }

    pub fn make_embed(&self) -> serenity::CreateEmbed {
        let mut embed = serenity::CreateEmbed::new()
            .title("The TEA House Moderator Election")
            .color(serenity::Color::BLURPLE)
            .field(
                "Candidates",
                self.candidates
                    .iter()
                    .map(|(n, r)| format!("* {n} (Region {r})"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );

        if !self.reserved_offices.is_empty() {
            embed = embed.field(
                "Reserved offices",
                self.reserved_offices
                    .iter()
                    .map(|v| format!("* {v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }

        if !self.ballots.is_empty() {
            embed = embed.field("Voters", format!("{}", self.ballots.len()), true);
        }

        embed
    }

    pub fn reserve_office<R: Into<Region>>(&mut self, region: R) -> bool {
        if self.reserved_offices.len() + 1 > self.offices {
            false
        } else {
            self.reserved_offices.push(region.into());
            true
        }
    }

    pub fn add_candidate<N: Into<Name>, R: Into<Region>>(&mut self, name: N, region: R) {
        self.candidates.insert(name.into(), region.into());
    }

    #[allow(unused)]
    pub fn vote<N: Into<Name>>(&mut self, user_id: serenity::UserId, name: N, rank: usize) {
        self.ballots
            .entry(user_id)
            .or_default()
            .votes
            .insert(name.into(), rank);
    }

    fn tally(&self) -> Vec<(f32, Name)> {
        let mut rng = rand::thread_rng();

        // Track the count of non-zero votes so that the total score can be normalized.
        let mut votes = HashMap::<Name, usize>::new();
        let mut results = HashMap::<Name, usize>::new();
        for ballot in self.ballots.values() {
            for (name, rank) in &ballot.votes {
                *results.entry(name.clone()).or_default() += rank;
                *votes.entry(name.clone()).or_default() += if *rank > 0 { 1 } else { 0 };
            }
        }
        let mut results: Vec<_> = results
            .into_iter()
            .map(|(n, v)| {
                let num_votes = *votes.get(&n).unwrap_or(&0);
                // Normalize the score for this candidate.
                (v as f32 / num_votes as f32, n)
            })
            .collect();
        results.shuffle(&mut rng);
        results.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        results
    }

    fn assign(&self, mut results: Vec<(f32, Name)>) -> Option<Vec<Name>> {
        let mut reserved_offices = self.reserved_offices.clone();
        let mut unreserved = self.offices - self.reserved_offices.len();
        let mut officers = Vec::new();

        while officers.len() < self.offices {
            let (_, candidate) = results.pop()?;
            tracing::info!("assigning {candidate} {officers:?}({})", self.offices);
            let region = self.candidates.get(&candidate).unwrap();

            if let Some(ix) = reserved_offices.iter().position(|x| x == region) {
                officers.push(candidate.clone());
                reserved_offices.remove(ix);
                tracing::warn!(
                    "{candidate} takes reserved office {region} ({})",
                    reserved_offices.len()
                );
            } else if unreserved > 0 {
                officers.push(candidate.clone());
                unreserved -= 1;
                tracing::warn!("{candidate} takes unreserved office {unreserved}");
            } else {
                tracing::warn!("Could not assign {candidate}");
            }
        }

        officers.sort();
        Some(officers)
    }

    pub fn run(&self) -> Option<Vec<Name>> {
        let results = self.tally();
        self.assign(results)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use test_case::test_case;

    #[test_case(
        vec![
            vec![("a", 1), ("b", 2), ("c", 3)]
        ],
        vec![("a", 1.), ("b", 2.), ("c", 3.)]; "single voter")]
    #[test_case(
        vec![
            vec![("a", 1), ("b", 2), ("c", 3)],
            vec![("a", 4), ("b", 5), ("c", 6)],
            vec![("a", 7), ("b", 8), ("c", 9)],
        ],
        vec![("a", 4.), ("b", 5.), ("c", 6.)]; "multiple voters")]
    #[test_case(
        vec![
            vec![("a", 1), ("b", 2), ("c", 3), ("d", 4)],
            vec![("a", 2), ("b", 3), ("c", 4), ("d", 1)],
            vec![("a", 3), ("b", 4), ("c", 1), ("d", 2)],
            vec![("a", 4), ("b", 1), ("c", 2), ("d", 3)],
        ],
        vec![("a", 2.5), ("b", 2.5), ("c", 2.5), ("d", 2.5)]; "vote ties")]
    #[test_case(
        vec![
            vec![("a", 2), ("b", 2), ("c", 2), ("d", 2)],
            vec![("a", 2), ("b", 0), ("c", 2), ("d", 0)],
            vec![("a", 2), ("b", 2), ("c", 2), ("d", 2)],
            vec![("a", 2), ("b", 2), ("c", 0), ("d", 0)],
            vec![("a", 2), ("b", 2), ("c", 0), ("d", 2)],
        ],
        vec![("a", 2.), ("b", 2.), ("c", 2.), ("d", 2.)]; "Allows abstention")]
    fn test_tally<N: Into<Name>>(votes: Vec<Vec<(N, usize)>>, expected: Vec<(N, f32)>) {
        let mut election = Election::new(1, 1);
        for (i, ballot) in votes.into_iter().enumerate() {
            for (n, rank) in ballot {
                election.vote((i as u64 + 1).into(), n, rank);
            }
        }
        let tally = election.tally();
        assert_eq!(tally.len(), expected.len());
        for (n, rank) in expected {
            assert!(tally.contains(&(rank, n.into())));
        }
    }

    #[test_case(
        4,
        vec!["AMER"],
        vec![(5., "a"), (7., "b"), (8., "d"), (9., "e"), (10., "c")],
        vec![("a", "AMER"), ("b", "EMEA"), ("c", "EMEA"), ("d", "EMEA"), ("e", "EMEA")],
        vec!["a", "c", "d", "e"];
        "low vote reserved"
    )]
    #[test_case(
        4,
        vec!["AMER", "EMEA"],
        vec![(5., "a"), (7., "b"), (8., "d"), (9., "e"), (10., "c")],
        vec![("a", "AMER"), ("b", "EMEA"), ("c", "EMEA"), ("d", "EMEA"), ("e", "AMER")],
        vec!["b", "c", "d", "e"];
        "simple"
    )]
    fn test_assign<N: Into<Name>, R: Into<Region>>(
        offices: usize,
        reservations: Vec<R>,
        tally: Vec<(f32, N)>,
        candidates: Vec<(N, R)>,
        expected: Vec<N>,
    ) {
        let mut election = Election::new(1, offices);
        for reserve in reservations {
            election.reserve_office(reserve);
        }
        for (n, r) in candidates {
            election.add_candidate(n, r);
        }

        let mut result = Vec::new();
        for (usize, n) in tally {
            result.push((usize, n.into()));
        }
        let expected = expected.into_iter().map(|n| n.into()).collect::<Vec<_>>();
        assert_eq!(Some(expected), election.assign(result));
    }

    #[test]
    fn test_run_election() {
        let mut election = Election::new(1, 4);
        election.reserve_office("AMER");
        election.reserve_office("EMEA");

        election.add_candidate("a", "AMER");
        election.add_candidate("b", "AMER");
        election.add_candidate("c", "AMER");
        election.add_candidate("d", "EMEA");
        election.add_candidate("e", "AMER");

        election.vote(1.into(), "a", 5);
        election.vote(1.into(), "b", 2);

        election.vote(2.into(), "a", 2);
        election.vote(2.into(), "d", 1);

        election.vote(3.into(), "c", 4);
        election.vote(3.into(), "e", 2);

        election.vote(4.into(), "e", 5);
        election.vote(4.into(), "b", 1);

        println!("{:?}", election.run());
    }
}
