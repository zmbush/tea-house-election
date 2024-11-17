#![deny(unused)]

use std::collections::HashMap;

use actions::{Action, ElectionAction, ElectionActionType, VoteAction, VoteActionType};
use anyhow::{anyhow, Context as _};
use chrono::{DateTime, TimeDelta, Utc};
use data::{GlobalData, GlobalState};
use either::Either;
use poise::{
    serenity_prelude::{
        self as serenity, CreateActionRow, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
        CreateSelectMenuOption, EditInteractionResponse,
    },
    CreateReply, FrameworkContext,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};
use tracing::warn;
use tracing_subscriber::{layer::SubscriberExt as _, Layer as _, Registry};

mod actions;
mod data;
mod election;

#[derive(Debug, Serialize, Deserialize)]
struct VoteInProgress {
    #[serde(default)]
    user: serenity::UserId,

    token: String,
    election: actions::ElectionId,
    election_message: serenity::MessageId,

    #[serde(default)]
    partial_ballot: election::Ballot,

    #[serde(default)]
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "version")]
enum Elections {
    #[serde(rename = "1")]
    V1(V1Elections),

    #[serde(rename = "2")]
    V2(V2Elections),
}

impl Default for Elections {
    fn default() -> Self {
        Elections::V2(Default::default())
    }
}

fn id_map<'de, D, Id, V>(deserializer: D) -> Result<HashMap<Id, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    Id: std::str::FromStr + Eq + std::hash::Hash,
    Id::Err: std::fmt::Debug,
    V: serde::Deserialize<'de>,
{
    let map = HashMap::<String, V>::deserialize(deserializer)?;
    let map: Result<HashMap<Id, V>, <Id as std::str::FromStr>::Err> =
        map.into_iter().map(|(k, v)| Ok((k.parse()?, v))).collect();
    map.map_err(|e| serde::de::Error::custom(format!("Could not parse {e:?}")))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct V1Elections {
    #[serde(default)]
    next_election_id: actions::ElectionId,
    #[serde(deserialize_with = "id_map")]
    elections: HashMap<actions::ElectionId, election::Election>,

    #[serde(default)]
    next_vote_id: actions::VoteId,
    #[serde(default, deserialize_with = "id_map")]
    votes_in_progress: HashMap<actions::VoteId, VoteInProgress>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ElectionMap {
    #[serde(default)]
    next_election_id: actions::ElectionId,
    #[serde(deserialize_with = "id_map")]
    elections: HashMap<actions::ElectionId, election::Election>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VoteMap {
    #[serde(default)]
    next_vote_id: actions::VoteId,
    #[serde(deserialize_with = "id_map")]
    votes: HashMap<actions::VoteId, VoteInProgress>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct V2Elections {
    elections: ElectionMap,
    votes: VoteMap,
}

impl Elections {
    fn latest(&mut self) -> &mut V2Elections {
        match self {
            Elections::V1(v1_elections) => {
                let mut v2 = V2Elections::default();
                v2.elections.next_election_id = v1_elections.next_election_id;
                std::mem::swap(&mut v2.elections.elections, &mut v1_elections.elections);

                v2.votes.next_vote_id = v1_elections.next_vote_id;
                std::mem::swap(&mut v2.votes.votes, &mut v1_elections.votes_in_progress);
                *self = Elections::V2(v2);
                self.latest()
            }
            Elections::V2(v2_elections) => {
                v2_elections.votes.expire();
                v2_elections
            }
        }
    }

    fn try_latest(&self) -> Option<&V2Elections> {
        match self {
            Elections::V2(v2_elections) => Some(v2_elections),
            _ => None,
        }
    }
}

impl VoteMap {
    fn start<EID: Into<actions::ElectionId>>(
        &mut self,
        election: EID,
        interaction: &serenity::ComponentInteraction,
    ) -> actions::VoteId {
        let vote_id = self.next_vote_id.next();
        self.votes.insert(
            vote_id,
            VoteInProgress {
                user: interaction.user.id,
                token: interaction.token.clone(),
                election: election.into(),
                election_message: interaction.message.id,

                expires_at: Utc::now() + TimeDelta::hours(1),
                partial_ballot: election::Ballot::default(),
            },
        );
        vote_id
    }

    fn remove<VID: Into<actions::VoteId>>(&mut self, vote: VID) -> Option<VoteInProgress> {
        self.votes.remove(&vote.into())
    }

    fn save_ballot<VID: Into<actions::VoteId>>(
        &mut self,
        vote: VID,
        elections: &mut ElectionMap,
    ) -> Result<(), anyhow::Error> {
        let vote = vote.into();
        let election = elections.get_mut(vote, self)?;
        let vote = self
            .votes
            .get_mut(&vote)
            .ok_or_else(|| anyhow!("Could not get vote in progress"))?;
        let mut ballot = election::Ballot::default();
        std::mem::swap(&mut ballot, &mut vote.partial_ballot);
        election.ballots.insert(vote.user, ballot);

        Ok(())
    }

    fn get<VID: Into<actions::VoteId>>(&self, vote: VID) -> Result<&VoteInProgress, anyhow::Error> {
        self.votes
            .get(&vote.into())
            .ok_or_else(|| anyhow!("No vote in progress!"))
    }

    fn get_mut<VID: Into<actions::VoteId>>(
        &mut self,
        vote: VID,
    ) -> Result<&mut VoteInProgress, anyhow::Error> {
        self.votes
            .get_mut(&vote.into())
            .ok_or_else(|| anyhow!("No vote in progress!"))
    }

    fn expire(&mut self) {
        let now = Utc::now();
        let mut old_votes = HashMap::new();
        std::mem::swap(&mut old_votes, &mut self.votes);
        self.votes = old_votes
            .into_iter()
            .filter(|(_, v)| v.expires_at > now)
            .collect();
    }
}

impl ElectionMap {
    fn get<ID: actions::ActionId>(
        &self,
        id: ID,
        vote_map: &VoteMap,
    ) -> Result<&election::Election, anyhow::Error> {
        match id.get_id() {
            Either::Left(election_id) => self
                .elections
                .get(election_id)
                .ok_or_else(|| anyhow!("No election found for this message")),
            Either::Right(vote_id) => {
                let election_id = vote_map.get(*vote_id)?.election;
                self.elections
                    .get(&election_id)
                    .ok_or_else(|| anyhow!("No election found for this message"))
            }
        }
    }

    fn get_mut<ID: actions::ActionId>(
        &mut self,
        id: ID,
        vote_map: &VoteMap,
    ) -> Result<&mut election::Election, anyhow::Error> {
        match id.get_id() {
            Either::Left(election_id) => self
                .elections
                .get_mut(election_id)
                .ok_or_else(|| anyhow!("No election found for this message")),
            Either::Right(vote_id) => {
                let election_id = vote_map.get(*vote_id)?.election;
                self.elections
                    .get_mut(&election_id)
                    .ok_or_else(|| anyhow!("No election found for this message"))
            }
        }
    }
}

impl V2Elections {
    async fn edit_response(
        &self,
        ctx: impl serenity::CacheHttp + Copy,
        vote: actions::VoteAction,
        interaction: &serenity::ComponentInteraction,
        edit_interaction: EditInteractionResponse,
    ) -> Result<(), anyhow::Error> {
        serenity::Builder::execute(edit_interaction, ctx, &self.votes.get(vote.vote_id)?.token)
            .await?;

        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;

        Ok(())
    }

    async fn update_election(
        &self,
        ctx: impl serenity::CacheHttp + Copy,
        action: actions::VoteAction,
        interaction: &serenity::ComponentInteraction,
    ) -> Result<(), anyhow::Error> {
        let edit = serenity::EditMessage::new()
            .embed(self.elections.get(action, &self.votes)?.make_embed());
        serenity::Builder::execute(
            edit,
            ctx,
            (
                interaction.channel_id,
                self.votes.get(action)?.election_message,
                None,
            ),
        )
        .await?;
        Ok(())
    }
}

impl data::Migrate for Elections {
    fn migrate(&mut self) {}
}

type Context<'a> = poise::Context<'a, data::GlobalState<Elections>, anyhow::Error>;

#[poise::command(slash_command, guild_only = true)]
async fn election(
    ctx: Context<'_>,
    offices: usize,
    reserved_offices: String,
    candidates: String,
) -> Result<(), anyhow::Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let mut data = ctx.data().write().await;
    let guild = data.guild_mut(guild_id);
    let guild = guild.latest();

    let mut election = election::Election::new(ctx.author(), offices);
    for office in reserved_offices.split(',') {
        if !election.reserve_office(office.trim()) {
            return Err(anyhow!("Too many office reservations"));
        }
    }
    for candidate in candidates.split(',') {
        let (candidate, region) = candidate
            .split_once(";")
            .ok_or_else(|| anyhow!("Could not split candidate {candidate} at ;"))?;
        election.add_candidate(candidate.trim(), region.trim());
    }

    let election_id = guild.elections.next_election_id.next();

    let reply = CreateReply::default()
        .embed(election.make_embed())
        .components(vec![CreateActionRow::Buttons(vec![
            Action::Election(ElectionAction {
                election_id,
                ty: ElectionActionType::InitiateVote,
            })
            .button(),
            Action::Election(ElectionAction {
                election_id,
                ty: ElectionActionType::GetResult,
            })
            .button(),
        ])]);
    ctx.send(reply).await?;
    guild.elections.elections.insert(election_id, election);
    data.persist("elections")?;

    Ok(())
}

fn vote_menu<VID: Into<actions::VoteId>>(vote_id: VID) -> Vec<CreateActionRow> {
    let vote_id = vote_id.into();
    vec![
        CreateActionRow::SelectMenu(CreateSelectMenu::new(
            Action::Vote(VoteAction {
                vote_id,
                ty: VoteActionType::SelectVote,
            }),
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new("1 (least desired)", "1"),
                    CreateSelectMenuOption::new("2", "2"),
                    CreateSelectMenuOption::new("3", "3"),
                    CreateSelectMenuOption::new("4", "4"),
                    CreateSelectMenuOption::new("5 (most desired)", "5"),
                ],
            },
        )),
        CreateActionRow::Buttons(vec![
            Action::Vote(VoteAction {
                vote_id,
                ty: VoteActionType::SkipVote,
            })
            .button(),
            Action::Vote(VoteAction {
                vote_id,
                ty: VoteActionType::VoidBallot,
            })
            .button(),
        ]),
    ]
}

async fn initiate_vote(
    ctx: &serenity::Context,
    action: actions::Action,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let guild = guild.latest();
    let (vote_id, confirmed) = match action {
        actions::Action::Election(actions::ElectionAction {
            election_id: id,
            ty: actions::ElectionActionType::InitiateVote,
        }) => (guild.votes.start(id, interaction), false),
        actions::Action::Vote(actions::VoteAction {
            vote_id: id,
            ty: actions::VoteActionType::ConfirmInitiateVote,
        }) => (id, true),
        _ => return Err(anyhow!("Invalid action for initiate_vote: {action:?}")),
    };
    let election = guild.elections.get_mut(action, &guild.votes)?;

    if election.ballots.contains_key(&interaction.user.id) && !confirmed {
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .ephemeral(true)
                        .content(
                            "You have already submitted a ballot. \
                            Voting again will overwrite your existing votes. Is this okay?",
                        )
                        .add_embed(election.ballots[&interaction.user.id].make_embed())
                        .button(
                            actions::Action::Vote(actions::VoteAction {
                                vote_id,
                                ty: actions::VoteActionType::ConfirmInitiateVote,
                            })
                            .button(),
                        )
                        .button(
                            actions::Action::Vote(actions::VoteAction {
                                vote_id,
                                ty: actions::VoteActionType::CancelVote,
                            })
                            .button(),
                        ),
                ),
            )
            .await?
    } else {
        let _: Option<_> = election.ballots.remove(&interaction.user.id);
        let (name, region) = election
            .candidates
            .iter()
            .next()
            .ok_or_else(|| anyhow!("No candidates!"))?;
        let content = format!("# Please vote for the candidate\n{name} (Region: {region})");
        match action {
            actions::Action::Election(_) => {
                interaction
                    .create_response(
                        ctx,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .content(content)
                                .components(vote_menu(vote_id)),
                        ),
                    )
                    .await?;
            }
            actions::Action::Vote(vote_action) => {
                guild
                    .edit_response(
                        ctx,
                        vote_action,
                        interaction,
                        EditInteractionResponse::new()
                            .content(content)
                            .embeds(vec![])
                            .components(vote_menu(vote_id)),
                    )
                    .await?
            }
        }
    }

    Ok(())
}

async fn select_vote(
    ctx: &serenity::Context,
    action: actions::VoteAction,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let guild = guild.latest();
    let election = guild.elections.get_mut(action, &guild.votes)?;
    let vote = guild.votes.get_mut(action)?;
    let mut needs_vote = false;
    let mut vote_registered = false;
    for (name, region) in &election.candidates {
        if !vote.partial_ballot.votes.contains_key(name) {
            if !vote_registered {
                vote_registered = true;
                if let serenity::ComponentInteractionDataKind::StringSelect { values } =
                    &interaction.data.kind
                {
                    let rank = values[0].parse()?;
                    vote.partial_ballot.votes.insert(name.clone(), rank);
                } else if action.ty == actions::VoteActionType::SkipVote {
                    vote.partial_ballot.votes.insert(name.clone(), 0);
                }
            } else {
                let content = format!("# Please vote for the candidate\n{name} (Region: {region})");
                needs_vote = true;
                guild
                    .edit_response(
                        ctx,
                        action,
                        interaction,
                        EditInteractionResponse::new()
                            .content(content)
                            .components(vote_menu(action)),
                    )
                    .await?;
                break;
            }
        }
    }

    if !needs_vote {
        guild
            .edit_response(
                ctx,
                action,
                interaction,
                EditInteractionResponse::new()
                    .content("Thank you for voting!")
                    .components(vec![]),
            )
            .await?;
        guild.votes.save_ballot(action, &mut guild.elections)?;
        guild.update_election(ctx, action, interaction).await?;
        guild.votes.remove(action);
        return Ok(());
    }

    Ok(())
}

async fn stop_vote(
    ctx: &serenity::Context,
    action: actions::VoteAction,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let guild = guild.latest();
    let election = guild.elections.get_mut(action, &guild.votes)?;

    if action.ty == actions::VoteActionType::VoidBallot {
        let _: Option<_> = election.ballots.remove(&interaction.user.id);
        guild
            .edit_response(
                ctx,
                action,
                interaction,
                EditInteractionResponse::new()
                    .content("Your vote has been deleted.\nUse the vote button to vote again!")
                    .components(vec![]),
            )
            .await?;
    } else {
        ctx.http
            .delete_original_interaction_response(&guild.votes.get(action)?.token)
            .await?;
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
    }
    guild.update_election(ctx, action, interaction).await?;
    guild.votes.remove(action);

    Ok(())
}

async fn get_result(
    ctx: &serenity::Context,
    action: actions::ElectionAction,
    interaction: &serenity::ComponentInteraction,
    data: &RwLockReadGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data
        .guild(guild_id)
        .ok_or_else(|| anyhow!("No guild data for this ID"))?;
    let guild = guild
        .try_latest()
        .ok_or_else(|| anyhow!("Guild data hasn't been upgraded"))?;

    let election = guild.elections.get(action, &guild.votes)?;
    if *election.owner() != interaction.user.id {
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .ephemeral(true)
                        .content("Only the creator of an election can view the results"),
                ),
            )
            .await?;
        return Ok(());
    }

    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .ephemeral(true)
                    .content(match election.run() {
                        Some(list) => format!(
                            "The following candidates have been elected:\n{}",
                            list.into_iter()
                                .map(|c| format!("* **{c}**"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ),
                        None => "Election did not complete. Likely there were not enough \
                            candidates to fill the required offices."
                            .into(),
                    }),
            ),
        )
        .await?;

    Ok(())
}

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: FrameworkContext<'_, data::GlobalState<Elections>, anyhow::Error>,
    data: &data::GlobalState<Elections>,
) -> Result<(), anyhow::Error> {
    if let serenity::FullEvent::InteractionCreate {
        interaction: serenity::Interaction::Component(interaction),
    } = event
    {
        if let Ok(action) = actions::Action::decode(&interaction.data.custom_id) {
            match action {
                actions::Action::Vote(vote_action) => match vote_action.ty {
                    actions::VoteActionType::ConfirmInitiateVote => {
                        let mut data = data.write().await;
                        initiate_vote(ctx, action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    actions::VoteActionType::SelectVote | actions::VoteActionType::SkipVote => {
                        let mut data = data.write().await;
                        select_vote(ctx, vote_action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    actions::VoteActionType::CancelVote | actions::VoteActionType::VoidBallot => {
                        let mut data = data.write().await;
                        stop_vote(ctx, vote_action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                },
                actions::Action::Election(election_action) => match election_action.ty {
                    actions::ElectionActionType::InitiateVote => {
                        let mut data = data.write().await;
                        initiate_vote(ctx, action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    actions::ElectionActionType::GetResult => {
                        let data = data.read().await;
                        get_result(ctx, election_action, interaction, &data).await?;
                    }
                },
            }
        } else {
            warn!("Cannot parse custom_id: {}", interaction.data.custom_id);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .max_log_files(10)
        .filename_prefix("rolling")
        .filename_suffix("log")
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .build("logs")
        .context("Can't make logger")?;

    let subscriber = Registry::default()
        .with(
            // Stdout
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(true)
                .with_filter(tracing::level_filters::LevelFilter::from_level(
                    tracing::Level::INFO,
                )),
        )
        .with(
            // Rolling logs
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(appender)
                .with_filter(
                    tracing_subscriber::filter::Targets::new()
                        .with_target("tea-house-election", tracing::Level::TRACE)
                        .with_default(tracing::Level::DEBUG),
                ),
        );

    tracing::subscriber::set_global_default(subscriber).context("subscriber setup")?;

    dotenv::dotenv().context("loading dotenv")?;

    let token = std::env::var("DISCORD_TOKEN")?;
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::<_, anyhow::Error>::builder()
        .options(poise::FrameworkOptions {
            commands: vec![election()],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                let mut results: GlobalData<Elections> =
                    if let Ok(contents) = std::fs::read_to_string("elections.json") {
                        serde_json::from_str(&contents)?
                    } else {
                        GlobalData::default()
                    };
                results.migrate();
                let _ = results.persist("elections");
                Ok(GlobalState::new(results))
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;
    client.unwrap().start().await.unwrap();

    Ok(())
}
