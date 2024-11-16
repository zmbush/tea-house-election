#![deny(unused)]

use std::collections::HashMap;

use anyhow::{anyhow, Context as _};
use data::{GlobalData, GlobalState};
use either::Either;
use election::Election;
use poise::{
    serenity_prelude::{
        self as serenity, CreateActionRow, CreateButton, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
        CreateSelectMenuOption, EditInteractionResponse, ReactionType,
    },
    CreateReply, FrameworkContext,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};
use tracing::warn;
use tracing_subscriber::{layer::SubscriberExt as _, Layer as _, Registry};

mod data;
mod election;

#[derive(Debug, Serialize, Deserialize)]
struct VoteInProgress {
    token: String,
    election: ElectionId,
    election_message: serenity::MessageId,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Elections {
    #[serde(default)]
    next_election_id: ElectionId,
    elections: HashMap<ElectionId, election::Election>,

    #[serde(default)]
    next_vote_id: VoteId,
    #[serde(default)]
    votes_in_progress: HashMap<VoteId, VoteInProgress>,
}

impl Elections {
    fn vote_start<EID: Into<ElectionId>>(
        &mut self,
        election: EID,
        interaction: &serenity::ComponentInteraction,
    ) -> VoteId {
        let vote_id = self.next_vote_id;
        self.next_vote_id.0 += 1;
        self.votes_in_progress.insert(
            vote_id,
            VoteInProgress {
                token: interaction.token.clone(),
                election: election.into(),
                election_message: interaction.message.id,
            },
        );
        vote_id
    }

    fn vote_complete<VID: Into<VoteId>>(&mut self, vote: VID) {
        self.votes_in_progress.remove(&vote.into());
    }

    fn get_vote_in_progress<VID: Into<VoteId>>(
        &self,
        vote: VID,
    ) -> Result<&VoteInProgress, anyhow::Error> {
        self.votes_in_progress
            .get(&vote.into())
            .ok_or_else(|| anyhow!("No vote in progress!"))
    }

    fn get_election<ID: ActionId>(&self, id: ID) -> Result<&election::Election, anyhow::Error> {
        match id.get_id() {
            Either::Left(election_id) => self
                .elections
                .get(election_id)
                .ok_or_else(|| anyhow!("No election found for this message")),
            Either::Right(vote_id) => {
                let election_id = self.get_vote_in_progress(*vote_id)?.election;
                self.elections
                    .get(&election_id)
                    .ok_or_else(|| anyhow!("No election found for this message"))
            }
        }
    }

    fn get_election_mut<ID: ActionId>(
        &mut self,
        id: ID,
    ) -> Result<&mut election::Election, anyhow::Error> {
        match id.get_id() {
            Either::Left(election_id) => self
                .elections
                .get_mut(election_id)
                .ok_or_else(|| anyhow!("No election found for this message")),
            Either::Right(vote_id) => {
                let election_id = self.get_vote_in_progress(*vote_id)?.election;
                self.elections
                    .get_mut(&election_id)
                    .ok_or_else(|| anyhow!("No election found for this message"))
            }
        }
    }

    async fn edit_response(
        &self,
        ctx: impl serenity::CacheHttp + Copy,
        vote: VoteAction,
        interaction: &serenity::ComponentInteraction,
        edit_interaction: EditInteractionResponse,
    ) -> Result<(), anyhow::Error> {
        serenity::Builder::execute(
            edit_interaction,
            ctx,
            &self.get_vote_in_progress(vote.vote_id)?.token,
        )
        .await?;

        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;

        Ok(())
    }

    async fn update_election(
        &self,
        ctx: impl serenity::CacheHttp + Copy,
        action: VoteAction,
        interaction: &serenity::ComponentInteraction,
    ) -> Result<(), anyhow::Error> {
        let edit = serenity::EditMessage::new().embed(self.get_election(action)?.make_embed());
        serenity::Builder::execute(
            edit,
            ctx,
            (
                interaction.channel_id,
                self.get_vote_in_progress(action)?.election_message,
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

trait ActionId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId>;
}

type Context<'a> = poise::Context<'a, data::GlobalState<Elections>, anyhow::Error>;

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct ElectionId(usize);

impl ActionId for ElectionId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        Either::Left(self)
    }
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct VoteId(usize);

impl ActionId for VoteId {
    fn get_id(&self) -> Either<&ElectionId, &VoteId> {
        Either::Right(self)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct ElectionAction {
    election_id: ElectionId,
    ty: ElectionActionType,
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
enum ElectionActionType {
    InitiateVote,
    GetResult,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct VoteAction {
    vote_id: VoteId,
    ty: VoteActionType,
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
enum VoteActionType {
    ConfirmInitiateVote,
    SelectVote,
    SkipVote,
    CancelVote,
    VoidBallot,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
enum Action {
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
    fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    fn decode<S: AsRef<str>>(s: S) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s.as_ref())
    }

    fn button(&self) -> CreateButton {
        let btn = CreateButton::new(*self);

        match self {
            Action::Election(ElectionAction { ty, .. }) => match ty {
                ElectionActionType::InitiateVote => {
                    btn.label("Vote!").emoji(ReactionType::Unicode("🗳️".into()))
                }
                ElectionActionType::GetResult => btn
                    .label("Results")
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(ReactionType::Unicode("🧮".into())),
            },
            Action::Vote(VoteAction { ty, .. }) => match ty {
                VoteActionType::ConfirmInitiateVote => btn
                    .label("Vote Again")
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(ReactionType::Unicode("🗳️".into())),
                VoteActionType::CancelVote => btn
                    .emoji(ReactionType::Unicode("✅".into()))
                    .style(serenity::ButtonStyle::Secondary)
                    .label("Keep Existing Votes"),

                VoteActionType::SkipVote => btn
                    .style(serenity::ButtonStyle::Secondary)
                    .emoji(ReactionType::Unicode("🤷".into()))
                    .label("Skip"),
                VoteActionType::VoidBallot => btn
                    .style(serenity::ButtonStyle::Danger)
                    .emoji(ReactionType::Unicode("🛑".into()))
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

    let mut election = Election::new(ctx.author(), offices);
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

    let election_id = guild.next_election_id;
    guild.next_election_id.0 += 1;

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
    let reply = ctx.send(reply).await?;
    let _message_id = reply.message().await?.id;
    guild.elections.insert(election_id, election);
    data.persist("elections")?;

    Ok(())
}

fn vote_menu<VID: Into<VoteId>>(vote_id: VID) -> Vec<CreateActionRow> {
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
    action: Action,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let (vote_id, confirmed) = match action {
        Action::Election(ElectionAction {
            election_id: id,
            ty: ElectionActionType::InitiateVote,
        }) => (guild.vote_start(id, interaction), false),
        Action::Vote(VoteAction {
            vote_id: id,
            ty: VoteActionType::ConfirmInitiateVote,
        }) => (id, true),
        _ => return Err(anyhow!("Invalid action for initiate_vote: {action:?}")),
    };
    let election = guild.get_election_mut(action)?;

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
                            Action::Vote(VoteAction {
                                vote_id,
                                ty: VoteActionType::ConfirmInitiateVote,
                            })
                            .button(),
                        )
                        .button(
                            Action::Vote(VoteAction {
                                vote_id,
                                ty: VoteActionType::CancelVote,
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
            Action::Election(_) => {
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
            Action::Vote(vote_action) => {
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
    action: VoteAction,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let election = guild.get_election_mut(action)?;
    let ballot = election.ballots.entry(interaction.user.id).or_default();
    let mut needs_vote = false;
    let mut vote_registered = false;
    for (name, region) in &election.candidates {
        if !ballot.votes.contains_key(name) {
            if !vote_registered {
                vote_registered = true;
                if let serenity::ComponentInteractionDataKind::StringSelect { values } =
                    &interaction.data.kind
                {
                    let vote = values[0].parse()?;
                    ballot.votes.insert(name.clone(), vote);
                } else if action.ty == VoteActionType::SkipVote {
                    ballot.votes.insert(name.clone(), 0);
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
        guild.vote_complete(action);
        return Ok(());
    }

    guild.update_election(ctx, action, interaction).await?;

    Ok(())
}

async fn stop_vote(
    ctx: &serenity::Context,
    action: VoteAction,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let election = guild.get_election_mut(action)?;

    if action.ty == VoteActionType::VoidBallot {
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
            .delete_original_interaction_response(&guild.get_vote_in_progress(action)?.token)
            .await?;
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
    }
    guild.update_election(ctx, action, interaction).await?;
    guild.vote_complete(action);

    Ok(())
}

async fn get_result(
    ctx: &serenity::Context,
    action: ElectionAction,
    interaction: &serenity::ComponentInteraction,
    data: &RwLockReadGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data
        .guild(guild_id)
        .ok_or_else(|| anyhow!("No guild data for this ID"))?;

    let election = guild.get_election(action)?;
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
                    .content(format!("{:?}", election.run())),
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
        if let Ok(action) = Action::decode(&interaction.data.custom_id) {
            match action {
                Action::Vote(vote_action) => match vote_action.ty {
                    VoteActionType::ConfirmInitiateVote => {
                        let mut data = data.write().await;
                        initiate_vote(ctx, action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    VoteActionType::SelectVote | VoteActionType::SkipVote => {
                        let mut data = data.write().await;
                        select_vote(ctx, vote_action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    VoteActionType::CancelVote | VoteActionType::VoidBallot => {
                        let mut data = data.write().await;
                        stop_vote(ctx, vote_action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                },
                Action::Election(election_action) => match election_action.ty {
                    ElectionActionType::InitiateVote => {
                        let mut data = data.write().await;
                        initiate_vote(ctx, action, interaction, &mut data).await?;
                        data.persist("elections")?;
                    }
                    ElectionActionType::GetResult => {
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
