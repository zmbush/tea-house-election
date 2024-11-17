use std::num::ParseIntError;

use either::Either;
use poise::serenity_prelude as serenity;
use serde::{Deserialize, Serialize};

pub(crate) trait ActionId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId>;
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct ElectionId(usize);

impl ElectionId {
    pub(crate) fn next(&mut self) -> Self {
        let ret = *self;
        self.0 += 1;
        ret
    }
}

impl ActionId for ElectionId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        Either::Left(self)
    }
}

impl std::str::FromStr for ElectionId {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ElectionId(s.parse()?))
    }
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct VoteId(usize);

impl VoteId {
    pub(crate) fn next(&mut self) -> Self {
        let ret = *self;
        self.0 += 1;
        ret
    }
}

impl ActionId for VoteId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        Either::Right(self)
    }
}

impl std::str::FromStr for VoteId {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(VoteId(s.parse()?))
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub(crate) struct ElectionAction {
    pub(crate) election_id: ElectionId,
    pub(crate) ty: ElectionActionType,
}

impl ActionId for ElectionAction {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        self.election_id.get_id()
    }
}

impl From<ElectionAction> for ElectionId {
    fn from(val: ElectionAction) -> Self {
        val.election_id
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ElectionActionType {
    InitiateVote,
    GetResult,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub(crate) struct VoteAction {
    pub(crate) vote_id: VoteId,
    pub(crate) ty: VoteActionType,
}

impl ActionId for VoteAction {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        self.vote_id.get_id()
    }
}

impl From<VoteAction> for VoteId {
    fn from(val: VoteAction) -> Self {
        val.vote_id
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum VoteActionType {
    ConfirmInitiateVote,
    SelectVote,
    SkipVote,
    CancelVote,
    VoidBallot,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub(crate) enum Action {
    Election(ElectionAction),
    Vote(VoteAction),
}

impl ActionId for Action {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        match self {
            Action::Election(election_action) => election_action.get_id(),
            Action::Vote(vote_action) => vote_action.get_id(),
        }
    }
}

impl Action {
    pub(crate) fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub(crate) fn decode<S: AsRef<str>>(s: S) -> Result<Self, serde_json::Error> {
        let s = s.as_ref();
        tracing::info!("Decoding {s}");
        serde_json::from_str(s)
    }

    pub(crate) fn button(&self) -> serenity::CreateButton {
        let btn = serenity::CreateButton::new(*self);

        match self {
            Action::Election(ElectionAction { ty, .. }) => match ty {
                ElectionActionType::InitiateVote => btn.label("Vote!").emoji(react("🗳️")),
                ElectionActionType::GetResult => btn
                    .label("Results")
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(react("🧮")),
            },
            Action::Vote(VoteAction { ty, .. }) => match ty {
                VoteActionType::ConfirmInitiateVote => btn
                    .label("Vote Again")
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(react("🗳️")),
                VoteActionType::CancelVote => btn
                    .emoji(react("✅"))
                    .style(serenity::ButtonStyle::Secondary)
                    .label("Keep Existing Votes"),

                VoteActionType::SkipVote => btn
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(react("🤷"))
                    .label("Skip"),
                VoteActionType::VoidBallot => btn
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(react("🛑"))
                    .label("Stop Voting"),

                VoteActionType::SelectVote => unimplemented!("SelectVote is not a button"),
            },
        }
    }
}

fn react(s: &str) -> serenity::ReactionType {
    s.try_into().expect("Invalid react emoji")
}

impl From<Action> for String {
    fn from(val: Action) -> Self {
        val.encode().expect("Could not encode action")
    }
}
