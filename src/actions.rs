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
        serde_json::from_str(s.as_ref())
    }

    pub(crate) fn button(&self) -> serenity::CreateButton {
        let btn = serenity::CreateButton::new(*self);

        match self {
            Action::Election(ElectionAction { ty, .. }) => match ty {
                ElectionActionType::InitiateVote => btn
                    .label("Vote!")
                    .emoji(serenity::ReactionType::Unicode("🗳️".into())),
                ElectionActionType::GetResult => btn
                    .label("Results")
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(serenity::ReactionType::Unicode("🧮".into())),
            },
            Action::Vote(VoteAction { ty, .. }) => match ty {
                VoteActionType::ConfirmInitiateVote => btn
                    .label("Vote Again")
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(serenity::ReactionType::Unicode("🗳️".into())),
                VoteActionType::CancelVote => btn
                    .emoji(serenity::ReactionType::Unicode("✅".into()))
                    .style(serenity::ButtonStyle::Secondary)
                    .label("Keep Existing Votes"),

                VoteActionType::SkipVote => btn
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(serenity::ReactionType::Unicode("🤷".into()))
                    .label("Skip"),
                VoteActionType::VoidBallot => btn
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(serenity::ReactionType::Unicode("🛑".into()))
                    .label("Stop Voting"),

                VoteActionType::SelectVote => unimplemented!("SelectVote is not a button"),
            },
        }
    }
}

impl From<Action> for String {
    fn from(val: Action) -> Self {
        val.encode().expect("Could not encode action")
    }
}
