// #![deny(unused)]

use std::collections::HashMap;

use anyhow::{anyhow, Context as _};
use data::{GlobalData, GlobalState};
use election::Election;
use poise::{
    serenity_prelude::{
        self as serenity, ComponentInteraction, CreateActionRow, CreateButton,
        CreateInteractionResponse, CreateInteractionResponseFollowup,
        CreateInteractionResponseMessage, CreateMessage, CreateModal, CreateQuickModal,
        CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, EditInteractionResponse,
        ReactionType,
    },
    CreateReply, FrameworkContext,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLockWriteGuard;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt as _, Layer as _, Registry};

mod data;
mod election;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Elections {
    elections: HashMap<serenity::MessageId, election::Election>,

    #[serde(default)]
    votes_in_progress:
        HashMap<serenity::ChannelId, HashMap<serenity::UserId, (String, serenity::MessageId)>>,
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

    let mut election = Election::new(offices);
    for office in reserved_offices.split(',') {
        if !election.reserve_office(office) {
            return Err(anyhow!("Too many office reservations"));
        }
    }
    for candidate in candidates.split(',') {
        let (candidate, region) = candidate
            .split_once(";")
            .ok_or_else(|| anyhow!("Could not split candidate {candidate} at ;"))?;
        election.add_candidate(candidate, region);
    }

    // ctx.reply(format!("Election: {election:?}")).await?;
    let reply = CreateReply::default()
        .content(format!("Election: {election:?}"))
        .components(vec![CreateActionRow::Buttons(vec![CreateButton::new(
            "initiate_vote",
        )
        .label("Vote!")
        .emoji(ReactionType::Unicode("🗳️".into()))])]);
    let reply = ctx.send(reply).await?;
    guild.elections.insert(reply.message().await?.id, election);
    data.persist("elections")?;

    Ok(())
}

fn vote_menu() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::SelectMenu(CreateSelectMenu::new(
            "select_vote",
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
            CreateButton::new("skip_vote")
                .style(serenity::ButtonStyle::Secondary)
                .emoji(ReactionType::Unicode("🤷".into()))
                .label("Skip"),
            CreateButton::new("void_ballot")
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

    let election = if interaction.data.custom_id == "initiate_vote" {
        guild
            .votes_in_progress
            .entry(interaction.channel_id)
            .or_default()
            .insert(
                interaction.user.id,
                (interaction.token.clone(), interaction.message.id),
            );
        guild
            .elections
            .get_mut(&interaction.message.id)
            .ok_or_else(|| anyhow!("No election associated with this message"))?
    } else {
        guild
            .elections
            .get_mut(
                &guild
                    .votes_in_progress
                    .get(&interaction.channel_id)
                    .unwrap()
                    .get(&interaction.user.id)
                    .unwrap()
                    .1,
            )
            .ok_or_else(|| anyhow!("No election associated with this message"))?
    };

    if election.ballots.contains_key(&interaction.user.id)
        && interaction.data.custom_id != "confirm_initiate_vote"
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
                        .button(
                            CreateButton::new("confirm_initiate_vote")
                                .label("Yes, I'm sure!")
                                .style(serenity::ButtonStyle::Primary)
                                .emoji(ReactionType::Unicode("🗳️".into())),
                        )
                        .button(
                            CreateButton::new("cancel_vote")
                                .emoji(ReactionType::Unicode("🛑".into()))
                                .style(serenity::ButtonStyle::Secondary)
                                .label("Nevermind"),
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
        if interaction.data.custom_id == "initiate_vote" {
            interaction
                .create_response(
                    ctx,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content(format!(
                                "# Please vote for the candidate\n{name} (Region: {region})"
                            ))
                            .components(vote_menu()),
                    ),
                )
                .await?;
        } else {
            serenity::Builder::execute(
                EditInteractionResponse::new()
                    .content(format!(
                        "# Please vote for the candidate\n{name} (Region: {region})"
                    ))
                    .components(vote_menu()),
                ctx,
                &guild
                    .votes_in_progress
                    .get(&interaction.channel_id)
                    .unwrap()
                    .get(&interaction.user.id)
                    .unwrap()
                    .0,
            )
            .await?;

            interaction
                .create_response(ctx, CreateInteractionResponse::Acknowledge)
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
    let (initial_interaction_id, vote_message) = guild
        .votes_in_progress
        .get(&interaction.channel_id)
        .ok_or_else(|| anyhow!("No votes in progress in this channel"))?
        .get(&interaction.user.id)
        .ok_or_else(|| anyhow!("No votes in progress for this user"))?;
    let election = guild
        .elections
        .get_mut(vote_message)
        .ok_or_else(|| anyhow!("No election associated with this message"))?;
    let ballot = election.ballots.entry(interaction.user.id).or_default();
    info!("{:#?}", interaction.data);
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
                } else if interaction.data.custom_id == "skip_vote" {
                    ballot.votes.insert(name.clone(), 0);
                }
            } else {
                needs_vote = true;
                serenity::Builder::execute(
                    EditInteractionResponse::new()
                        .content(format!(
                            "# Please vote for the candidate\n{name} (Region: {region})"
                        ))
                        .components(vote_menu()),
                    ctx,
                    initial_interaction_id,
                )
                .await?;
                break;
            }
        }
    }
    if !needs_vote {
        serenity::Builder::execute(
            EditInteractionResponse::new()
                .content("Thank you for voting!")
                .components(vec![]),
            ctx,
            initial_interaction_id,
        )
        .await?;
        guild
            .votes_in_progress
            .get_mut(&interaction.channel_id)
            .unwrap()
            .remove(&interaction.user.id);
        return Ok(());
    }

    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;

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
    let (initial_interaction_id, vote_message) = guild
        .votes_in_progress
        .get(&interaction.channel_id)
        .ok_or_else(|| anyhow!("No votes in progress in this channel"))?
        .get(&interaction.user.id)
        .ok_or_else(|| anyhow!("No votes in progress for this user"))?;
    let election = guild
        .elections
        .get_mut(vote_message)
        .ok_or_else(|| anyhow!("No election associated with this message"))?;

    if interaction.data.custom_id == "void_vote" {
        let _: Option<_> = election.ballots.remove(&interaction.user.id);
        serenity::Builder::execute(
            EditInteractionResponse::new()
                .content("Your votes have been deleted.\nUse the vote button to vote again!")
                .components(vec![]),
            ctx,
            initial_interaction_id,
        )
        .await?;
    } else {
        ctx.http
            .delete_original_interaction_response(initial_interaction_id)
            .await?;
    }
    guild
        .votes_in_progress
        .get_mut(&interaction.channel_id)
        .unwrap()
        .remove(&interaction.user.id);

    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
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
            "initiate_vote" | "confirm_initiate_vote" => {
                initiate_vote(ctx, interaction, &mut data).await?
            }
            "select_vote" | "skip_vote" => select_vote(ctx, interaction, &mut data).await?,
            "cancel_vote" | "void_ballot" => stop_vote(ctx, interaction, &mut data).await?,
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
