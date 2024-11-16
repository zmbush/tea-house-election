#![deny(unused)]

use std::collections::HashMap;

use anyhow::{anyhow, Context as _};
use data::{GlobalData, GlobalState};
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
use tokio::sync::RwLockWriteGuard;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt as _, Layer as _, Registry};

mod data;
mod election;

#[derive(Debug, Serialize, Deserialize)]
struct VoteInProgress {
    token: String,
    election_message: serenity::MessageId,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Elections {
    elections: HashMap<serenity::MessageId, election::Election>,

    #[serde(default)]
    votes_in_progress: HashMap<serenity::ChannelId, HashMap<serenity::UserId, VoteInProgress>>,
}

impl Elections {
    fn vote_start(&mut self, interaction: &serenity::ComponentInteraction) {
        self.votes_in_progress
            .entry(interaction.channel_id)
            .or_default()
            .insert(
                interaction.user.id,
                VoteInProgress {
                    token: interaction.token.clone(),
                    election_message: interaction.message.id,
                },
            );
    }

    fn vote_complete(&mut self, interaction: &serenity::ComponentInteraction) {
        self.votes_in_progress
            .get_mut(&interaction.channel_id)
            .and_then(|c| c.remove(&interaction.user.id));
    }

    fn get_vote_in_progress(
        &self,
        interaction: &serenity::ComponentInteraction,
    ) -> Result<&VoteInProgress, anyhow::Error> {
        self.votes_in_progress
            .get(&interaction.channel_id)
            .ok_or_else(|| anyhow!("No vote in progress!"))?
            .get(&interaction.user.id)
            .ok_or_else(|| anyhow!("No vote in progress!"))
    }

    fn get_election(
        &self,
        interaction: &serenity::ComponentInteraction,
    ) -> Result<&election::Election, anyhow::Error> {
        self.elections
            .get(&interaction.message.id)
            .ok_or_else(|| anyhow!("No election found for this message"))
            .or_else(|_| {
                let msg = self.get_vote_in_progress(interaction)?.election_message;
                self.elections
                    .get(&msg)
                    .ok_or_else(|| anyhow!("No election associated with this message"))
            })
    }

    fn get_election_mut(
        &mut self,
        interaction: &serenity::ComponentInteraction,
    ) -> Result<&mut election::Election, anyhow::Error> {
        let msg = self.get_vote_in_progress(interaction)?.election_message;
        self.elections
            .get_mut(&msg)
            .ok_or_else(|| anyhow!("No election associated with this message"))
    }

    async fn edit_response(
        &self,
        ctx: impl serenity::CacheHttp + Copy,
        interaction: &serenity::ComponentInteraction,
        edit_interaction: EditInteractionResponse,
    ) -> Result<(), anyhow::Error> {
        serenity::Builder::execute(
            edit_interaction,
            ctx,
            &self.get_vote_in_progress(interaction)?.token,
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
        interaction: &serenity::ComponentInteraction,
    ) -> Result<(), anyhow::Error> {
        let edit = serenity::EditMessage::new().embed(self.get_election(interaction)?.make_embed());
        serenity::Builder::execute(
            edit,
            ctx,
            (
                interaction.channel_id,
                self.get_vote_in_progress(interaction)?.election_message,
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

const INITIATE_VOTE: &str = "initiate_vote";
const GET_RESULT: &str = "get_vote_result";
const CONFIRM_INITIATE_VOTE: &str = "confirm_initiate_vote";
const SELECT_VOTE: &str = "select_vote";
const SKIP_VOTE: &str = "skip_vote";
const CANCEL_VOTE: &str = "cancel_vote";
const VOID_BALLOT: &str = "void_ballot";

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

    let reply = CreateReply::default()
        .embed(election.make_embed())
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(INITIATE_VOTE)
                .label("Vote!")
                .emoji(ReactionType::Unicode("🗳️".into())),
            CreateButton::new(GET_RESULT)
                .label("Results")
                .style(serenity::ButtonStyle::Secondary)
                .emoji(ReactionType::Unicode("🧮".into())),
        ])]);
    let reply = ctx.send(reply).await?;
    guild.elections.insert(reply.message().await?.id, election);
    data.persist("elections")?;

    Ok(())
}

fn vote_menu() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::SelectMenu(CreateSelectMenu::new(
            SELECT_VOTE,
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
            CreateButton::new(SKIP_VOTE)
                .style(serenity::ButtonStyle::Secondary)
                .emoji(ReactionType::Unicode("🤷".into()))
                .label("Skip"),
            CreateButton::new(VOID_BALLOT)
                .style(serenity::ButtonStyle::Danger)
                .emoji(ReactionType::Unicode("🛑".into()))
                .label("Stop Voting"),
        ]),
    ]
}

async fn initiate_vote(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    if interaction.data.custom_id == INITIATE_VOTE {
        guild.vote_start(interaction);
    }
    let election = guild.get_election_mut(interaction)?;

    if election.ballots.contains_key(&interaction.user.id)
        && interaction.data.custom_id != CONFIRM_INITIATE_VOTE
    {
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
                            CreateButton::new(CONFIRM_INITIATE_VOTE)
                                .label("Vote Again")
                                .style(serenity::ButtonStyle::Danger)
                                .emoji(ReactionType::Unicode("🗳️".into())),
                        )
                        .button(
                            CreateButton::new(CANCEL_VOTE)
                                .emoji(ReactionType::Unicode("✅".into()))
                                .style(serenity::ButtonStyle::Secondary)
                                .label("Keep Existing Votes"),
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
        if interaction.data.custom_id == INITIATE_VOTE {
            interaction
                .create_response(
                    ctx,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content(content)
                            .components(vote_menu()),
                    ),
                )
                .await?;
        } else {
            guild
                .edit_response(
                    ctx,
                    interaction,
                    EditInteractionResponse::new()
                        .content(content)
                        .embeds(vec![])
                        .components(vote_menu()),
                )
                .await?;
        };
    }

    Ok(())
}

async fn select_vote(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let election = guild.get_election_mut(interaction)?;
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
                } else if interaction.data.custom_id == SKIP_VOTE {
                    ballot.votes.insert(name.clone(), 0);
                }
            } else {
                let content = format!("# Please vote for the candidate\n{name} (Region: {region})");
                needs_vote = true;
                guild
                    .edit_response(
                        ctx,
                        interaction,
                        EditInteractionResponse::new()
                            .content(content)
                            .components(vote_menu()),
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
                interaction,
                EditInteractionResponse::new()
                    .content("Thank you for voting!")
                    .components(vec![]),
            )
            .await?;
        guild.vote_complete(interaction);
        return Ok(());
    }

    guild.update_election(ctx, interaction).await?;

    Ok(())
}

async fn stop_vote(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);
    let election = guild.get_election_mut(interaction)?;

    if interaction.data.custom_id == "void_vote" {
        let _: Option<_> = election.ballots.remove(&interaction.user.id);
        guild
            .edit_response(
                ctx,
                interaction,
                EditInteractionResponse::new()
                    .content("Your vote has been cancelled.\nUse the vote button to vote again!")
                    .components(vec![]),
            )
            .await?;
    } else {
        ctx.http
            .delete_original_interaction_response(&guild.get_vote_in_progress(interaction)?.token)
            .await?;
    }
    guild.update_election(ctx, interaction).await?;
    guild.vote_complete(interaction);

    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;

    Ok(())
}

async fn get_result(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &mut RwLockWriteGuard<'_, data::GlobalData<Elections>>,
) -> Result<(), anyhow::Error> {
    let guild_id = interaction
        .guild_id
        .ok_or_else(|| anyhow::anyhow!("No guild id. Must be in a guild"))?;
    let guild = data.guild_mut(guild_id);

    let election = guild.get_election(interaction)?;
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
        let mut data = data.write().await;

        match interaction.data.custom_id.as_str() {
            INITIATE_VOTE | CONFIRM_INITIATE_VOTE => {
                initiate_vote(ctx, interaction, &mut data).await?
            }
            SELECT_VOTE | SKIP_VOTE => select_vote(ctx, interaction, &mut data).await?,
            CANCEL_VOTE | VOID_BALLOT => stop_vote(ctx, interaction, &mut data).await?,
            GET_RESULT => get_result(ctx, interaction, &mut data).await?,
            other => info!("Unknown custom_id: {other}"),
        }

        data.persist("elections")?;
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
