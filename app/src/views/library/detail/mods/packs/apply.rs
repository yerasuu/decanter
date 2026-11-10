//! Applying a pack, as a plan shown before anything is fetched (design, sections 5.3, 6.2 and
//! 8.2): what will be downloaded and how much, what is already installed, what needs a file the
//! user brings, what is not available, the resolved order and the winners it changes among the
//! installed mods. Thunderstore entries download through the community's listing; Nexus Mods
//! entries are a manual step (a Premium account downloads one per click, since batch downloads
//! are still a question for Nexus); mod.io entries are a manual step too, since its downloads need
//! the terms dialog and access checks of its browser; local entries ask for their archive.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use decanter_modpack::{
    Availability, Dependency, Entry, EntryKey, LocalNotes, Pack, PackId, Plan, ResolveError, Source, Status, StepState,
    Strength, Track,
};
use decanter_mods::{GameMods, ModId, OrderChange};
use decanter_thunderstore::{Listing, Package, PackageVersion};
use gpui::{Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::run::{self, Job, Providers, Run, Summary, Work};
use super::{attribution, entry_name, read_notes, version_text};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::ModsTab;
use crate::views::library::detail::mods::toggles::install_choice;
use crate::widgets::{ButtonKind, button, progress_bar};

const TICK: Duration = Duration::from_millis(200);
/// Failures and conflicts a summary names before summing up the rest.
const SHOWN_LINES: usize = 12;

pub(super) enum Apply {
    Preparing,
    Ready(Box<Planned>),
    Running { planned: Box<Planned>, work: Arc<Work> },
}

impl Apply {
    pub(super) const fn is_working(&self) -> bool {
        matches!(self, Self::Preparing | Self::Running { .. })
    }

    pub(super) fn cancel(&self) {
        if let Self::Running { work, .. } = self {
            work.cancel();
        }
    }
}

pub(super) struct Planned {
    pack: Pack,
    notes: LocalNotes,
    plan: Plan,
    rows: Vec<Row>,
    /// Winners that placing the present entries in the pack's block changes among the mods on now.
    change: OrderChange,
    names: HashMap<ModId, String>,
    warnings: Vec<Text>,
    providers: Arc<Providers>,
    summary: Option<Summary>,
    /// Some entry leaves it to the installer whether it ends on.
    has_unstated: bool,
}

/// One step of the plan, as the dialog shows it.
struct Row {
    key: EntryKey,
    name: String,
    entry: Entry,
    size: Option<u64>,
    state: StepState,
    blocked_by: Option<String>,
    held_by: Option<String>,
    page: Option<String>,
    can_download: bool,
}

impl ModsTab {
    pub(super) fn start_apply(&mut self, id: PackId, cx: &mut Context<Self>) {
        let enable_unstated = self.enables_on_install();
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        packs.apply = Some(Apply::Preparing);
        packs.error = None;
        packs.notice = None;
        let account = self.target.account_id.clone();
        let nexus_on = self.nexus_experimental;
        tracing::info!(pack = %id, "mod pack plan asked");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let prepared =
                tokio::spawn(async move { prepare(mods, account, id, nexus_on, enable_unstated).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(packs) = this.packs.as_mut() else {
                    return;
                };
                if !matches!(packs.apply, Some(Apply::Preparing)) {
                    return;
                }
                packs.apply = None;
                match prepared {
                    Ok(Ok(planned)) => packs.apply = Some(Apply::Ready(Box::new(planned))),
                    Ok(Err(text)) => {
                        tracing::warn!(error = %text, "mod pack plan not made");
                        packs.set_error(text);
                    }
                    Err(err) => {
                        tracing::error!(%err, "making the mod pack plan stopped");
                        packs.set_error(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_job(&mut self, job: Job, cx: &mut Context<Self>) {
        let enable_unstated = self.enables_on_install();
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let Some(Apply::Ready(planned)) = packs.apply.take() else {
            return;
        };
        let account = self.target.account_id.clone();
        let work = Arc::new(Work::default());
        let run = Run {
            mods: mods.clone(),
            account: account.clone(),
            pack: planned.pack.clone(),
            notes: planned.notes.clone(),
            plan: planned.plan.clone(),
            providers: Arc::clone(&planned.providers),
            work: Arc::clone(&work),
        };
        let providers = Arc::clone(&planned.providers);
        let pack_id = planned.pack.id.clone();
        packs.apply = Some(Apply::Running { planned, work });
        Self::follow_apply(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let summary = tokio::spawn(run::execute(run, job)).await;
            let replanned = tokio::task::spawn_blocking(move || {
                let pack =
                    Pack::read(&decanter_modpack::pack_file(mods.dir(), &pack_id)).map_err(super::read_failed)?;
                let notes = read_notes(&mods, &pack.id).unwrap_or_else(|| LocalNotes::new(pack.id.clone()));
                plan_for(&mods, &account, pack, notes, providers, enable_unstated)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.apply = None;
                    match (summary, replanned) {
                        (Ok(summary), Ok(Ok(mut planned))) => {
                            planned.summary = Some(summary);
                            packs.apply = Some(Apply::Ready(Box::new(planned)));
                        }
                        (_, Ok(Err(text))) => packs.set_error(text),
                        (Err(err), _) => {
                            tracing::error!(%err, "mod pack apply task stopped");
                            packs.set_error(Text::verbatim(err));
                        }
                        (_, Err(err)) => {
                            tracing::error!(%err, "replanning the mod pack stopped");
                            packs.set_error(Text::verbatim(err));
                        }
                    }
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The plan again with the install choice changed, which entries that don't say follow.
    fn replan_with_choice(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.set_enable_on_install(enabled, cx);
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let Some(Apply::Ready(planned)) = packs.apply.as_ref() else {
            return;
        };
        let (pack, notes, providers) = (
            planned.pack.clone(),
            planned.notes.clone(),
            Arc::clone(&planned.providers),
        );
        let account = self.target.account_id.clone();
        packs.apply = Some(Apply::Preparing);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let replanned =
                tokio::task::spawn_blocking(move || plan_for(&mods, &account, pack, notes, providers, enabled)).await;
            let _ = this.update(cx, |this, cx| {
                let Some(packs) = this.packs.as_mut() else {
                    return;
                };
                if !matches!(packs.apply, Some(Apply::Preparing)) {
                    return;
                }
                packs.apply = None;
                match replanned {
                    Ok(Ok(planned)) => packs.apply = Some(Apply::Ready(Box::new(planned))),
                    Ok(Err(text)) => packs.set_error(text),
                    Err(err) => {
                        tracing::error!(%err, "replanning the mod pack stopped");
                        packs.set_error(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_for_entry(key: EntryKey, wants_folder: bool, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let dialog = rfd::AsyncFileDialog::new().set_title(t!("mods.packs.pick_title"));
            let picked = if wants_folder {
                dialog.pick_folder().await
            } else {
                dialog
                    .add_filter(t!("mods.archive.filter"), &["zip", "ZIP", "7z", "7Z"])
                    .pick_file()
                    .await
            };
            let Some(picked) = picked else {
                return;
            };
            let path = picked.path().to_path_buf();
            tracing::info!(entry = %key, path = %path.display(), "file picked for a pack entry");
            let _ = this.update(cx, |this, cx| this.run_job(Job::File { key, path }, cx));
        })
        .detach();
    }

    fn follow_apply(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = this.update(cx, |this, cx| {
                    cx.notify();
                    this.packs
                        .as_ref()
                        .and_then(|packs| packs.apply.as_ref())
                        .is_some_and(|apply| matches!(apply, Apply::Running { .. }))
                });
                if !matches!(working, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn apply_panel(apply: &Apply, enable_on_install: bool, locked: bool, cx: &Context<Self>) -> Div {
        match apply {
            Apply::Preparing => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(theme::muted(t!("mods.packs.preparing")))
                .child(div().flex().child(
                    button("pack-plan-stop", ButtonKind::Secondary, t!("common.cancel"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(packs) = this.packs.as_mut() {
                                packs.apply = None;
                            }
                            cx.notify();
                        },
                    )),
                )),
            Apply::Running { planned, work } => running_panel(planned, work, cx),
            Apply::Ready(planned) => plan_panel(planned, enable_on_install, locked, cx),
        }
    }
}

/// What the plan needs from the providers, read once on the user's click, then the plan itself.
async fn prepare(
    mods: GameMods,
    account: String,
    id: PackId,
    nexus_on: bool,
    enable_unstated: bool,
) -> Result<Planned, Text> {
    let reader = mods.clone();
    let (pack, notes, installed) = tokio::task::spawn_blocking(move || {
        let pack = Pack::read(&decanter_modpack::pack_file(reader.dir(), &id)).map_err(super::read_failed)?;
        let notes = read_notes(&reader, &pack.id).unwrap_or_else(|| LocalNotes::new(pack.id.clone()));
        let installed = reader.list().map_err(Text::verbatim)?;
        Ok::<_, Text>((pack, notes, installed))
    })
    .await
    .map_err(Text::verbatim)??;
    let diff = decanter_modpack::diff(&pack, &installed, Some(&notes));
    let lacks = |wanted: fn(&Source) -> bool| {
        pack.entries
            .iter()
            .any(|entry| wanted(&entry.source) && !matches!(diff.status(&entry.key), Some(Status::Present { .. })))
    };
    let mut warnings = Vec::new();
    let thunderstore = if lacks(|source| matches!(source, Source::Thunderstore { .. })) {
        thunderstore_provider(&mods).await.unwrap_or_else(|text| {
            warnings.push(text);
            None
        })
    } else {
        None
    };
    let nexus = if nexus_on && lacks(|source| matches!(source, Source::Nexus { .. })) {
        match nexus_provider().await {
            Ok(nexus) => Some(nexus),
            Err(err) => {
                tracing::info!(%err, "nexus account not read for the pack, its entries stay manual");
                warnings.push(Text::new("mods.packs.nexus_account").text("reason", crate::nexus::failure_text(&err)));
                None
            }
        }
    } else {
        None
    };
    if !nexus_on && lacks(|source| matches!(source, Source::Nexus { .. })) {
        warnings.push(Text::new("mods.packs.nexus_off"));
    }
    if lacks(|source| matches!(source, Source::Modio { .. })) {
        warnings.push(Text::new("mods.packs.modio_manual"));
    }
    let providers = Arc::new(Providers {
        thunderstore,
        nexus,
        warnings,
    });
    tokio::task::spawn_blocking(move || plan_for(&mods, &account, pack, notes, providers, enable_unstated))
        .await
        .map_err(Text::verbatim)?
}

/// `None` when the game has no community chosen, which leaves its Thunderstore entries waiting.
async fn thunderstore_provider(mods: &GameMods) -> Result<Option<run::Thunderstore>, Text> {
    let reader = mods.clone();
    let settings = tokio::task::spawn_blocking(move || decanter_thunderstore::GameSettings::read(&reader))
        .await
        .map_err(Text::verbatim)?
        .map_err(listing_failed)?;
    let Some(settings) = settings else {
        return Err(Text::new("mods.packs.no_community"));
    };
    let cache = decanter_thunderstore::cache_dir(mods).ok_or_else(|| Text::new("mods.packs.no_listing"))?;
    let client = decanter_thunderstore::Client::new().map_err(listing_failed)?;
    let listing = decanter_thunderstore::listing(
        &client,
        &cache,
        &settings.community,
        decanter_thunderstore::Refresh::IfStale,
    )
    .await
    .map_err(listing_failed)?;
    Ok(Some(run::Thunderstore {
        client,
        listing: Arc::new(listing),
        community: settings.community,
    }))
}

fn listing_failed(err: decanter_thunderstore::Error) -> Text {
    Text::new("mods.packs.listing_failed").arg("reason", err)
}

async fn nexus_provider() -> decanter_nexus::Result<run::Nexus> {
    let client = crate::nexus::client().await?;
    let account = decanter_nexus::validate(&client).await?;
    Ok(run::Nexus {
        client,
        is_premium: account.is_premium,
    })
}

/// The entry's package and version in the listing: the pinned one, or the latest for a `latest`
/// entry; `None` when the listing no longer has it.
pub(super) fn thunderstore_version<'listing>(
    listing: &'listing Listing,
    entry: &Entry,
) -> Option<(&'listing Package, &'listing PackageVersion)> {
    let Source::Thunderstore { package, version } = &entry.source else {
        return None;
    };
    let found = listing.get(&package.parse().ok()?)?;
    if entry.track == Track::Latest {
        return Some((found, &found.latest));
    }
    Some((found, found.version(version.parse().ok()?)?))
}

/// Off the UI: the plan from what the game has now, so a second run does only what remains. An
/// entry that doesn't say whether it ends on takes `enable_unstated`, the installer's choice.
fn plan_for(
    mods: &GameMods,
    account: &str,
    mut pack: Pack,
    notes: LocalNotes,
    providers: Arc<Providers>,
    enable_unstated: bool,
) -> Result<Planned, Text> {
    let mut has_unstated = false;
    for entry in pack.entries.iter_mut().filter(|entry| !entry.is_enabled_stated) {
        entry.enabled = enable_unstated;
        has_unstated = true;
    }
    let installed = mods.list().map_err(Text::verbatim)?;
    let diff = decanter_modpack::diff(&pack, &installed, Some(&notes));
    let mut warnings = providers.warnings.clone();
    let dependencies = providers
        .thunderstore
        .as_ref()
        .map(|thunderstore| thunderstore_dependencies(&pack, &thunderstore.listing, &installed, &mut warnings))
        .unwrap_or_default();
    let name_of_key = |key: &EntryKey| {
        pack.entry(key)
            .and_then(|entry| entry.name.clone())
            .unwrap_or_else(|| key.to_string())
    };
    let resolution = decanter_modpack::resolve(&pack, &dependencies).map_err(|err| match err {
        ResolveError::Cycle(cycle) => Text::new("mods.packs.cycle").arg(
            "entries",
            cycle.entries.iter().map(&name_of_key).collect::<Vec<_>>().join(", "),
        ),
        other => Text::verbatim(other),
    })?;
    if !resolution.moves().is_empty() {
        warnings.push(Text::new("mods.packs.reordered").arg("count", resolution.moves().len()));
    }
    for dropped in resolution.dropped() {
        tracing::warn!(before = %dropped.before, after = %dropped.after, "provider dependency dropped to break a cycle");
    }
    if let (Some(wanted), Some(thunderstore)) = (pack.game.thunderstore.as_deref(), providers.thunderstore.as_ref())
        && wanted != thunderstore.community.to_string()
    {
        warnings.push(
            Text::new("mods.packs.community_differs")
                .arg("pack", wanted)
                .arg("game", &thunderstore.community),
        );
    }
    let plan = Plan::new(&pack, &resolution, &diff, |entry| availability(entry, &providers)).map_err(Text::verbatim)?;
    let rows = plan
        .steps()
        .iter()
        .filter_map(|step| {
            let entry = pack.entry(&step.key)?.clone();
            let listed = providers
                .thunderstore
                .as_ref()
                .and_then(|thunderstore| thunderstore_version(&thunderstore.listing, &entry));
            let page = match &entry.source {
                Source::Nexus { domain, mod_id, .. } => domain
                    .parse::<decanter_nexus::GameDomain>()
                    .ok()
                    .zip(decanter_nexus::NexusModId::new(*mod_id))
                    .map(|(domain, id)| decanter_nexus::mod_page(&domain, id)),
                Source::Thunderstore { .. } => listed.and_then(|(package, _)| package.page.clone()),
                _ => None,
            };
            let waits = matches!(
                step.state,
                StepState::Manual { .. } | StepState::Failed { .. } | StepState::Unavailable { .. }
            );
            let can_download = waits
                && step.blocked_by.is_none()
                && matches!(entry.source, Source::Nexus { .. })
                && providers.nexus.as_ref().is_some_and(|nexus| nexus.is_premium);
            Some(Row {
                key: step.key.clone(),
                name: entry_name(&entry),
                size: listed.and_then(|(_, version)| version.file_size).or(entry.size),
                state: step.state.clone(),
                blocked_by: step.blocked_by.as_ref().map(&name_of_key),
                held_by: step.held_by.as_ref().map(&name_of_key),
                page,
                can_download,
                entry,
            })
        })
        .collect();
    let members: Vec<ModId> = plan
        .steps()
        .iter()
        .filter_map(|step| match &step.state {
            StepState::Have { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let change = run::reorder_change(mods, account, &members).map_err(Text::verbatim)?;
    let names = installed
        .into_iter()
        .map(|manifest| (manifest.id, manifest.name))
        .collect();
    Ok(Planned {
        pack,
        notes,
        plan,
        rows,
        change,
        names,
        warnings,
        providers,
        summary: None,
        has_unstated,
    })
}

fn availability(entry: &Entry, providers: &Providers) -> Availability {
    match &entry.source {
        Source::Thunderstore { .. } => {
            if providers
                .thunderstore
                .as_ref()
                .and_then(|thunderstore| thunderstore_version(&thunderstore.listing, entry))
                .is_some()
            {
                Availability::Fetch
            } else {
                Availability::Unavailable
            }
        }
        _ => Availability::Manual,
    }
}

/// The listing's dependencies between the pack's Thunderstore entries, hard edges for the resolver;
/// one on a package neither in the pack nor installed is said, since the mod may not work without it.
fn thunderstore_dependencies(
    pack: &Pack,
    listing: &Listing,
    installed: &[decanter_mods::ModManifest],
    warnings: &mut Vec<Text>,
) -> Vec<Dependency> {
    let by_package: HashMap<&str, &EntryKey> = pack
        .entries
        .iter()
        .filter_map(|entry| match &entry.source {
            Source::Thunderstore { package, .. } => Some((package.as_str(), &entry.key)),
            _ => None,
        })
        .collect();
    let have = decanter_thunderstore::installed(installed);
    let mut dependencies = Vec::new();
    let mut said = HashSet::new();
    for entry in &pack.entries {
        let Some((_, version)) = thunderstore_version(listing, entry) else {
            continue;
        };
        for wanted in &version.dependencies {
            let package = wanted.package.to_string();
            match by_package.get(package.as_str()) {
                Some(&required) if *required != entry.key => {
                    dependencies.push(Dependency::new(entry.key.clone(), required.clone(), Strength::Hard));
                }
                Some(_) => {}
                None => {
                    if have.get(&wanted.package).is_none() && said.insert(package) {
                        warnings.push(
                            Text::new("mods.packs.dependency_missing")
                                .arg("name", entry_name(entry))
                                .arg("dependency", wanted),
                        );
                    }
                }
            }
        }
    }
    dependencies
}

fn state_text(row: &Row) -> Text {
    if let Some(required) = row.blocked_by.as_ref() {
        return Text::new("mods.packs.state_blocked").arg("name", required);
    }
    let text = match &row.state {
        StepState::Have { adopted: true, .. } => Text::new("mods.packs.state_have"),
        StepState::Have { adopted: false, .. } => Text::new("mods.packs.state_installed"),
        StepState::Fetch { replaces: Some(_) } => Text::new("mods.packs.state_update"),
        StepState::Fetch { replaces: None } => Text::new("mods.packs.state_fetch"),
        StepState::Unavailable { .. } => Text::new("mods.packs.state_unavailable"),
        StepState::Failed { .. } => Text::new("mods.packs.state_failed"),
        StepState::Manual { .. } | _ => Text::new("mods.packs.state_manual"),
    };
    match (row.held_by.as_ref(), row.entry.enabled) {
        (Some(required), _) => Text::new("mods.packs.state_held")
            .text("state", text)
            .arg("name", required),
        (None, false) => Text::new("mods.packs.state_off").text("state", text),
        (None, true) => text,
    }
}

fn plan_panel(planned: &Planned, enable_on_install: bool, locked: bool, cx: &Context<ModsTab>) -> Div {
    let count = |wanted: fn(&StepState) -> bool| planned.rows.iter().filter(|row| wanted(&row.state)).count();
    let fetch_rows = planned
        .rows
        .iter()
        .filter(|row| row.blocked_by.is_none() && matches!(row.state, StepState::Fetch { .. }));
    let fetch_bytes = fetch_rows
        .clone()
        .filter_map(|row| row.size)
        .fold(0_u64, u64::saturating_add);
    let counts = t!(
        "mods.packs.plan_counts",
        fetch = fetch_rows.count(),
        size = crate::format::bytes(fetch_bytes),
        have = count(|state| matches!(state, StepState::Have { .. })),
        manual = count(|state| matches!(state, StepState::Manual { .. } | StepState::Failed { .. })),
        unavailable = count(|state| matches!(state, StepState::Unavailable { .. }))
    );
    let names = &planned.names;
    let name_of = |id: &ModId| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let change = super::edit::change_lines(&planned.change, &name_of);
    let rows = planned
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| plan_row(index, row, locked, cx));
    let waiting = planned.plan.waiting();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().child(t!("mods.packs.plan_title", name = planned.pack.name)))
        .child(theme::muted(t!("mods.packs.plan_trust")).text_size(px(12.5)))
        .children(
            planned
                .warnings
                .iter()
                .map(|text| theme::muted(text).text_size(px(12.5))),
        )
        .children(planned.summary.as_ref().map(summary_block))
        .child(theme::muted(counts))
        .children((waiting > 0).then(|| {
            theme::muted(t!(
                "mods.packs.incomplete",
                waiting = waiting,
                count = planned.plan.steps().len()
            ))
        }))
        .children((!change.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(12.5))
                .child(theme::muted(t!("mods.packs.plan_changes")))
                .children(change.into_iter().map(|line| theme::muted(line).pl_2()))
        }))
        .child(theme::card().children(rows))
        .children((planned.has_unstated && !locked).then(|| {
            install_choice(
                "pack-enable-on-install",
                enable_on_install,
                t!("mods.toggles.install_enabled_pack").into_owned().into(),
                ModsTab::replan_with_choice,
                cx,
            )
        }))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(
                    button(
                        "pack-plan-apply",
                        ButtonKind::Primary,
                        t!("mods.packs.apply_confirm"),
                        locked,
                    )
                    .when(!locked, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.run_job(Job::Fetch, cx)))
                    }),
                )
                .child(
                    button(
                        "pack-plan-close",
                        ButtonKind::Secondary,
                        t!("mods.packs.close_plan"),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(packs) = this.packs.as_mut() {
                            packs.apply = None;
                        }
                        cx.notify();
                    })),
                ),
        )
}

fn plan_row(index: usize, row: &Row, locked: bool, cx: &Context<ModsTab>) -> Div {
    let waits = row.blocked_by.is_none()
        && matches!(
            row.state,
            StepState::Manual { .. } | StepState::Failed { .. } | StepState::Unavailable { .. }
        );
    let wants_folder = matches!(row.entry.source, Source::Local { .. }) && row.entry.sha256.is_none();
    let manual = waits.then(|| {
        let (pick_key, download_key) = (row.key.clone(), row.key.clone());
        let page = row.page.clone();
        let pick_label = if wants_folder {
            t!("mods.packs.pick_folder")
        } else {
            t!("mods.packs.pick_file")
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .pl(px(29.))
            .children(manual_hint(row).map(|hint| theme::muted(hint).text_size(px(12.5))))
            .children(page.map(|url| {
                button(
                    ("pack-plan-page", index),
                    ButtonKind::Secondary,
                    t!("mods.packs.open_page"),
                    false,
                )
                .on_click(move |_, _, _| {
                    if let Err(err) = crate::desktop::open(&url) {
                        tracing::warn!(%err, "could not open a pack entry's page");
                    }
                })
            }))
            .child(
                button(("pack-plan-pick", index), ButtonKind::Secondary, pick_label, locked).when(!locked, |b| {
                    b.on_click(cx.listener(move |_, _, _, cx| {
                        ModsTab::pick_for_entry(pick_key.clone(), wants_folder, cx);
                    }))
                }),
            )
            .children(row.can_download.then(|| {
                button(
                    ("pack-plan-download", index),
                    ButtonKind::Secondary,
                    t!("mods.packs.download"),
                    locked,
                )
                .when(!locked, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.run_job(
                            Job::NexusDownload {
                                key: download_key.clone(),
                            },
                            cx,
                        );
                    }))
                })
            }))
    });
    let size = row.size.map(crate::format::bytes);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px(px(12.))
        .py(px(8.))
        .border_b_1()
        .border_color(gpui::rgb(theme::LINE))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(theme::muted(t!("mods.order.slot", slot = index.saturating_add(1))).min_w(px(24.)))
                .child(div().flex_1().min_w_0().child(row.name.clone()))
                .child(theme::muted(SharedString::from(&state_text(row))).text_size(px(12.5))),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .pl(px(29.))
                .text_size(px(12.5))
                .child(attribution(&row.entry))
                .child(theme::muted(version_text(&row.entry)))
                .children(size.map(theme::muted)),
        )
        .children(manual)
}

/// What the user does by hand for a waiting entry, with the file it wants.
fn manual_hint(row: &Row) -> Option<SharedString> {
    let text = match &row.entry.source {
        Source::Nexus { .. } => t!("mods.packs.manual_nexus"),
        Source::Modio { mod_id, file, .. } => t!("mods.packs.manual_modio", id = mod_id, file = file),
        Source::Local { file_name } => t!("mods.packs.manual_local", file = file_name),
        Source::Thunderstore { package, version } => {
            t!("mods.packs.manual_thunderstore", package = package, version = version)
        }
        _ => return None,
    };
    Some(text.into_owned().into())
}

fn summary_block(summary: &Summary) -> Div {
    let failed = summary.failed.iter().take(SHOWN_LINES).map(|(name, reason)| {
        theme::error(
            Text::new("mods.packs.failed_line")
                .arg("name", name)
                .text("reason", reason.clone()),
        )
        .text_size(px(12.5))
    });
    let rest = summary.failed.len().saturating_sub(SHOWN_LINES);
    let conflicts_rest = summary
        .conflicts
        .len()
        .saturating_sub(SHOWN_LINES)
        .saturating_add(summary.more_conflicts);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().child(t!(
            "mods.packs.run_done",
            installed = summary.installed.len(),
            failed = summary.failed.len()
        )))
        .children(summary.cancelled.then(|| theme::muted(t!("mods.packs.run_cancelled"))))
        .children(failed)
        .children((rest > 0).then(|| theme::muted(t!("library.detail.mods.more", count = rest))))
        .children((!summary.conflicts.is_empty()).then(|| theme::muted(t!("mods.packs.conflicts_title"))))
        .children(
            summary
                .conflicts
                .iter()
                .take(SHOWN_LINES)
                .map(|line| theme::muted(line.clone()).pl_2().text_size(px(12.5))),
        )
        .children((conflicts_rest > 0).then(|| theme::muted(t!("library.detail.mods.more", count = conflicts_rest))))
}

fn running_panel(planned: &Planned, work: &Arc<Work>, cx: &Context<ModsTab>) -> Div {
    let current = work.current.load(core::sync::atomic::Ordering::Relaxed);
    let name = planned
        .rows
        .get(current)
        .map_or_else(String::new, |row| row.name.clone());
    let stopping = work.is_cancelled();
    let snapshot = work
        .thunderstore
        .snapshot()
        .or_else(|| work.nexus.snapshot())
        .or_else(|| work.progress.snapshot());
    let text = if stopping {
        t!("mods.archive.cancelling")
    } else {
        t!(
            "mods.packs.running",
            name = name,
            current = current.saturating_add(1),
            count = planned.rows.len()
        )
    };
    let cancel = Arc::clone(work);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(theme::muted(text))
        .child(progress_bar(
            "pack-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(div().flex().child(
            button("pack-stop", ButtonKind::Secondary, t!("common.cancel"), stopping).when(!stopping, |b| {
                b.on_click(cx.listener(move |_, _, _, cx| {
                    cancel.cancel();
                    cx.notify();
                }))
            }),
        ))
}
