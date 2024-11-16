use std::{collections::BTreeMap, path::Path};

use anyhow::Context as _;
use chrono::Utc;
use poise::serenity_prelude as serenity;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub trait Migrate {
    fn migrate(&mut self);
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct GlobalData<GuildData> {
    guilds: BTreeMap<serenity::GuildId, GuildData>,
}

impl<GuildData> GlobalData<GuildData> {
    pub fn migrate(&mut self)
    where
        GuildData: Migrate,
    {
        self.guilds.values_mut().for_each(Migrate::migrate);
    }

    pub fn guild(&self, id: serenity::GuildId) -> Option<&GuildData> {
        self.guilds.get(&id)
    }

    pub fn guild_mut(&mut self, guild_id: serenity::GuildId) -> &mut GuildData
    where
        GuildData: Default,
    {
        self.guilds.entry(guild_id).or_default()
    }
}

fn persist_folder<S: AsRef<str>, P: AsRef<Path>, P2: AsRef<Path>>(
    path_base: S,
    folder: P,
    filename: P2,
    keep: usize,
) -> std::io::Result<()> {
    let src_file = format!("{}.json", path_base.as_ref());
    let folder = folder.as_ref();
    std::fs::create_dir_all(folder)?;
    if !Path::new(&src_file).is_file() {
        return Ok(());
    }
    std::fs::copy(src_file, folder.join(filename))?;
    let mut existing: Vec<_> = std::fs::read_dir(folder)?.collect::<Result<_, _>>()?;
    existing.sort_by_key(|f| f.path());

    let count = existing.len();
    if count > keep {
        for file in existing.into_iter().take(count - keep) {
            std::fs::remove_file(file.path())?;
        }
    }

    Ok(())
}

impl<GuildData> GlobalData<GuildData> {
    pub fn persist<S: AsRef<str>>(&self, path_base: S) -> Result<(), anyhow::Error>
    where
        GuildData: Serialize,
    {
        let path_base = path_base.as_ref();
        let now = Utc::now();
        persist_folder(
            path_base,
            "bku/history",
            format!("{}-{}.json", path_base, now.timestamp()),
            20,
        )?;

        let mut output = std::fs::File::create(format!("{}.json", path_base))
            .context("while opening data file")?;
        serde_json::to_writer_pretty(&mut output, self).context("while formatting json")?;

        persist_folder(
            path_base,
            "bku/hourly",
            format!("{}-{}.json", path_base, now.timestamp() / 60 / 60),
            24,
        )?;
        persist_folder(
            path_base,
            "bku/daily",
            format!("{}-{}.json", path_base, now.timestamp() / 60 / 60 / 24),
            30,
        )?;
        persist_folder(
            path_base,
            "bku/monthly",
            format!("{}-{}.json", path_base, now.timestamp() / 60 / 60 / 24 / 28),
            usize::MAX,
        )?;

        Ok(())
    }
}

pub struct GlobalState<GuildData> {
    data: RwLock<GlobalData<GuildData>>,
}

impl<D> GlobalState<D> {
    pub fn new(data: GlobalData<D>) -> Self {
        Self {
            data: RwLock::new(data),
        }
    }

    #[allow(unused)]
    pub async fn read(&self) -> RwLockReadGuard<'_, GlobalData<D>> {
        self.data.read().await
    }

    pub async fn write(&self) -> RwLockWriteGuard<'_, GlobalData<D>> {
        self.data.write().await
    }
}
