//! A pack made from what a mod site shares: a Thunderstore profile code (r2modman's and the
//! Thunderstore Mod Manager's) or a Nexus Mods collection address. The site's list becomes an
//! ordinary pack of pinned entries in its order; applying it is the pack flow as any other, so
//! every download is checked and imported the way an archive is.

use std::collections::HashSet;

use decanter_modpack::{Entry, Pack, Source};
use decanter_mods::GameMods;
use gpui::Context;

use super::create::unique_key;
use super::edit::save_failed;
use super::{Packs, open_data};
use crate::i18n::Text;
use crate::views::library::detail::mods::ModsTab;

const MAX_DESCRIPTION_CHARS: usize = 400;

enum Shared {
    Thunderstore(decanter_thunderstore::ProfileCode),
    Nexus(decanter_nexus::CollectionRef),
}

fn recognise(pasted: &str) -> Option<Shared> {
    if let Ok(code) = pasted.parse() {
        return Some(Shared::Thunderstore(code));
    }
    decanter_nexus::parse_collection_reference(pasted)
        .ok()
        .map(Shared::Nexus)
}

impl Packs {
    pub(super) fn is_importing(&self) -> bool {
        self.web_open
    }
}

impl ModsTab {
    pub(super) fn start_web_import(&mut self, cx: &mut Context<Self>) {
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        packs.web_open = true;
        packs
            .web_input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        cx.notify();
    }

    pub(super) fn cancel_web_import(&mut self, cx: &mut Context<Self>) {
        if let Some(packs) = self.packs.as_mut() {
            packs.web_open = false;
        }
        cx.notify();
    }

    pub(super) fn import_from_web(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let nexus_on = self.nexus_experimental;
        let title = self.target.title.clone();
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let pasted = packs.web_input.read(cx).text().trim().to_owned();
        let Some(shared) = recognise(&pasted) else {
            packs.set_error(Text::new("mods.packs.web_unknown"));
            cx.notify();
            return;
        };
        if matches!(shared, Shared::Nexus(_)) && !nexus_on {
            packs.set_error(Text::new("mods.packs.web_nexus_off"));
            cx.notify();
            return;
        }
        packs.web_open = false;
        packs.busy = Some(Text::new("mods.packs.web_reading"));
        packs.error = None;
        packs.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let made = tokio::spawn(async move { fetch(&mods, shared, &title).await }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.busy = None;
                }
                match made {
                    Ok(Ok(pack)) => this.keep_imported(pack, cx),
                    Ok(Err(text)) => {
                        tracing::warn!(error = %text, "pack import from the web failed");
                        if let Some(packs) = this.packs.as_mut() {
                            packs.set_error(text);
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, "pack import from the web stopped");
                        if let Some(packs) = this.packs.as_mut() {
                            packs.set_error(Text::verbatim(err));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn keep_imported(&mut self, pack: Pack, cx: &mut Context<Self>) {
        self.packs_task(
            Text::new("mods.packs.saving"),
            move |mods, _| {
                pack.write(&decanter_modpack::pack_file(mods.dir(), &pack.id))
                    .map_err(save_failed)?;
                tracing::info!(pack = %pack.id, entries = pack.entries.len(), "mod pack imported from the web");
                open_data(mods, pack)
            },
            |this, open, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.notice = Some(
                        Text::new("mods.packs.web_imported")
                            .arg("name", &open.pack.name)
                            .arg("count", open.pack.entries.len()),
                    );
                    packs.open = Some(open);
                }
                cx.notify();
            },
            cx,
        );
    }
}

async fn fetch(mods: &GameMods, shared: Shared, title: &str) -> Result<Pack, Text> {
    match shared {
        Shared::Thunderstore(code) => from_profile(mods, &code, title).await,
        Shared::Nexus(reference) => from_collection(mods, &reference, title).await,
    }
}

fn blank(name: &str, title: &str, mods: &GameMods) -> Pack {
    let mut pack = Pack::new(name.to_owned());
    pack.game.title = Some(title.to_owned()).filter(|title| !title.is_empty());
    pack.game.thunderstore = decanter_thunderstore::GameSettings::read(mods)
        .ok()
        .flatten()
        .map(|settings| settings.community.to_string());
    pack.game.nexus = decanter_nexus::GameSettings::read(mods)
        .ok()
        .flatten()
        .map(|settings| settings.domain.to_string());
    pack
}

fn order_of(index: usize) -> u32 {
    u32::try_from(index)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
        .saturating_mul(10)
}

async fn from_profile(mods: &GameMods, code: &decanter_thunderstore::ProfileCode, title: &str) -> Result<Pack, Text> {
    let client = decanter_thunderstore::Client::new().map_err(profile_failed)?;
    let profile = decanter_thunderstore::profile(&client, code)
        .await
        .map_err(profile_failed)?;
    let mut pack = blank(&profile.name, title, mods);
    pack.description = Some(format!("Thunderstore profile {code}"));
    let mut keys = HashSet::new();
    for (index, listed) in profile.mods.iter().enumerate() {
        let Some(key) = unique_key(listed.package.name(), &mut keys) else {
            continue;
        };
        let mut entry = Entry::new(
            key,
            order_of(index),
            Source::Thunderstore {
                package: listed.package.to_string(),
                version: listed.version.to_string(),
            },
        );
        entry.enabled = listed.enabled;
        entry.name = Some(listed.package.name().to_owned());
        entry.author = Some(listed.package.namespace().to_owned());
        pack.entries.push(entry);
    }
    tracing::info!(pack = %pack.id, entries = pack.entries.len(), "pack read from a thunderstore profile code");
    Ok(pack)
}

async fn from_collection(
    mods: &GameMods,
    reference: &decanter_nexus::CollectionRef,
    title: &str,
) -> Result<Pack, Text> {
    let client = crate::nexus::client()
        .await
        .map_err(|err| crate::nexus::failure_text(&err))?;
    let collection = decanter_nexus::collection(&client, reference)
        .await
        .map_err(|err| crate::nexus::failure_text(&err))?;
    let mapped = decanter_nexus::GameSettings::read(mods).ok().flatten();
    if let Some(settings) = mapped.filter(|settings| settings.domain != collection.domain) {
        return Err(Text::new("mods.packs.web_other_game")
            .arg("collection", &collection.domain)
            .arg("game", &settings.domain));
    }
    let mut pack = blank(&collection.name, title, mods);
    pack.game.nexus = Some(collection.domain.to_string());
    pack.description = Some(
        [
            collection
                .author
                .as_ref()
                .map(|author| format!("Nexus Mods collection by {author}")),
            collection.summary.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(". ")
        .chars()
        .take(MAX_DESCRIPTION_CHARS)
        .collect(),
    )
    .filter(|text: &String| !text.is_empty());
    let mut keys = HashSet::new();
    for (index, listed) in collection.files.iter().enumerate() {
        let Some(key) = unique_key(&listed.name, &mut keys) else {
            continue;
        };
        let mut entry = Entry::new(
            key,
            order_of(index),
            Source::Nexus {
                domain: collection.domain.to_string(),
                mod_id: listed.mod_id.get(),
                file: listed.file.get(),
                version: listed.version.clone(),
            },
        );
        entry.enabled = !listed.optional;
        entry.name = Some(listed.name.clone());
        entry.size = listed.size;
        pack.entries.push(entry);
    }
    tracing::info!(
        pack = %pack.id,
        entries = pack.entries.len(),
        revision = collection.revision,
        "pack read from a nexus collection"
    );
    Ok(pack)
}

fn profile_failed(err: decanter_thunderstore::Error) -> Text {
    match err {
        decanter_thunderstore::Error::NotFound { .. } => Text::new("mods.packs.web_code_missing"),
        other => Text::new("mods.packs.web_failed").arg("reason", other),
    }
}
