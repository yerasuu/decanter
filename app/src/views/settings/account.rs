//! The profile's storefront accounts — decanter holds every session itself
//! now, so this is where one gets added or dropped, no next-ui, no
//! plugin. A profile can hold several, even several of the same
//! storefront: each shows its own row, with its own "Cerrar sesión".
//!
//! Also where the profile's library lives, since moving it is a
//! per-profile concern too, which library is the active one, and adding
//! another person, now that the topbar only shows profiles once there are
//! two.

use std::path::{Path, PathBuf};

use decanter_core::{Account, Profile, Storefront};
use gpui::{
    AsyncApp, Context, Entity, FontWeight, IntoElement, SharedString, Subscription, WeakEntity, Window, div,
    prelude::*, px,
};
use rust_i18n::t;

use super::{column, field_row, inline, storefront_label};
use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::views::gog_login::{GogLogin, LoggedIn};
use crate::views::profile_form::{self, ProfileForm};
use crate::views::{session_renewal, shell};
use crate::widgets::{ButtonKind, Select, button};

#[derive(Clone, PartialEq)]
enum AccountStatus {
    Idle,
    Working,
    Failed(Text),
}

/// The upstream session check behind a row's label.
enum SessionCheck {
    Pending,
    Valid(String),
    Closed,
    Unreachable,
    Failed(String),
}

struct AccountRow {
    account: Account,
    session: SessionCheck,
    status: AccountStatus,
}

pub struct AccountTab {
    profile_id: String,
    library_path: Option<String>,
    moving: bool,
    move_progress: Option<(u64, u64)>,
    move_error: Option<Text>,
    libraries: Vec<(PathBuf, Option<Vec<Profile>>)>,
    active_root: Option<PathBuf>,
    switch_error: Option<Text>,
    new_profile: Option<(Entity<ProfileForm>, Subscription)>,
    rows: Vec<AccountRow>,
    adding: bool,
    login: Entity<GogLogin>,
}

impl AccountTab {
    pub fn new(profile_id: String, cx: &mut Context<Self>) -> Self {
        let login = cx.new(GogLogin::new);
        cx.subscribe(&login, |this, _, LoggedIn(account), cx| this.connect(account, cx))
            .detach();
        let mut this = Self {
            profile_id,
            library_path: None,
            moving: false,
            move_progress: None,
            move_error: None,
            libraries: crate::core::profiles_by_library(),
            active_root: decanter_core::bootstrap::active_root(),
            switch_error: None,
            new_profile: None,
            rows: Vec::new(),
            adding: false,
            login,
        };
        this.reload_library_path();
        this.reload_accounts(cx);
        this
    }

    fn reload_library_path(&mut self) {
        self.library_path = crate::core::library_for_profile(&self.profile_id)
            .ok()
            .map(|library| library.root().display().to_string());
    }

    /// Re-lists the profile's accounts, keeping each surviving row's state
    /// so only a newly-appeared account gets its session checked.
    fn reload_accounts(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.as_str();
        let accounts = match crate::core::db() {
            Ok(db) => db
                .accounts_in_profile(profile_id)
                .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to list accounts"))
                .unwrap_or_default(),
            Err(err) => {
                tracing::warn!(profile_id, error = %err, "failed to open db, account list not refreshed");
                return;
            }
        };
        let mut previous = std::mem::take(&mut self.rows);
        self.rows = accounts
            .into_iter()
            .map(|account| {
                match previous
                    .iter()
                    .position(|row| row.account.id == account.id && row.account.storefront == account.storefront)
                {
                    Some(index) => {
                        let mut row = previous.swap_remove(index);
                        row.account = account;
                        row
                    }
                    None => {
                        Self::check_session(&account, cx);
                        AccountRow {
                            account,
                            session: SessionCheck::Pending,
                            status: AccountStatus::Idle,
                        }
                    }
                }
            })
            .collect();
        cx.notify();
    }

    fn row_mut(&mut self, account_id: &str, storefront: Storefront) -> Option<&mut AccountRow> {
        self.rows
            .iter_mut()
            .find(|row| row.account.id == account_id && row.account.storefront == storefront)
    }

    /// Re-checked when the row first appears, so a session GOG has since
    /// closed shows as such instead of a dead "connected" state.
    fn check_session(account: &Account, cx: &mut Context<Self>) {
        let account_id = account.id.clone();
        let storefront = account.storefront;
        cx.spawn(async move |this, cx| {
            tracing::debug!(account_id, "checking gog session status");
            let check = match decanter_gog::check_session(&account_id).await {
                Ok(decanter_gog::SessionState::Valid(session)) => SessionCheck::Valid(session.display_name),
                Ok(decanter_gog::SessionState::Closed) => {
                    tracing::info!(account_id, "gog session closed upstream");
                    SessionCheck::Closed
                }
                Ok(decanter_gog::SessionState::Unreachable(reason)) => {
                    tracing::info!(account_id, reason, "gog session could not be checked");
                    SessionCheck::Unreachable
                }
                Err(err) => {
                    tracing::error!(account_id, error = %err, "gog session check failed");
                    SessionCheck::Failed(err.to_string())
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(row) = this.row_mut(&account_id, storefront) {
                    row.session = check;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_row_status(
        this: &WeakEntity<Self>,
        account_id: &str,
        storefront: Storefront,
        status: AccountStatus,
        cx: &mut AsyncApp,
    ) {
        let _ = this.update(cx, |this, cx| {
            if let Some(row) = this.row_mut(account_id, storefront) {
                row.status = status;
                cx.notify();
            }
        });
    }

    fn log_out(&mut self, account_id: String, storefront: Storefront, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(&account_id, storefront) {
            row.status = AccountStatus::Working;
        }
        cx.notify();
        tracing::info!(account_id, "logging out of gog");
        let library = AppState::get(cx).library.clone();
        cx.spawn(async move |this, cx| {
            // Storefront-specific logout is the only part that varies
            // here — GOG is the only one implemented today, but a future
            // storefront would branch on `storefront` at this one spot, not
            // rewrite the row around it.
            if let Err(err) = decanter_gog::log_out(&account_id).await {
                tracing::error!(account_id, error = %err, "gog logout failed");
                Self::set_row_status(
                    &this,
                    &account_id,
                    storefront,
                    AccountStatus::Failed(Text::verbatim(err)),
                    cx,
                );
                return;
            }
            match crate::core::db().and_then(|db| {
                db.forget_account(&account_id, storefront)
                    .map_err(crate::core::StoreError::Query)
            }) {
                Ok(()) => {
                    tracing::info!(account_id, "account removed from profile");
                    // Otherwise a fetch task left running past logout
                    // survives a same-session re-login under the same
                    // account id too — see `GogLibrary::forget_account`'s
                    // own doc for why.
                    library.update(cx, |library, cx| library.forget_account(&account_id, cx));
                    let _ = this.update(cx, |this, cx| this.reload_accounts(cx));
                }
                Err(err) => {
                    tracing::error!(account_id, error = %err, "failed to remove account from profile");
                    Self::set_row_status(
                        &this,
                        &account_id,
                        storefront,
                        AccountStatus::Failed(err.localized()),
                        cx,
                    );
                }
            }
        })
        .detach();
    }

    /// No account id went into the login — which account this is is only
    /// known once GOG answers, so `profile_id` (already known) is what ties
    /// the *result* to this profile, via `upsert_account`.
    fn connect(&mut self, account: &decanter_gog::Account, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.as_str();
        match session_renewal::save_account(account, profile_id) {
            Ok(()) => {
                tracing::info!(profile_id, account_id = %account.account_id, "gog account added");
                AppState::get(cx)
                    .library
                    .clone()
                    .update(cx, |library, cx| library.refresh(cx));
                self.login.update(cx, GogLogin::reset);
                self.adding = false;
                self.reload_accounts(cx);
            }
            Err(err) => self.login.update(cx, |login, cx| login.fail(err, cx)),
        }
    }

    /// Runs the renewal window's flow for this row, starting straight at the
    /// silent refresh since the user already asked for it here.
    fn renew(&self, account_id: &str, storefront: Storefront, cx: &mut Context<Self>) {
        let Some(row) = self
            .rows
            .iter()
            .find(|row| row.account.id == account_id && row.account.storefront == storefront)
        else {
            return;
        };
        let Some(view) = session_renewal::open(row.account.clone(), true, cx) else {
            return;
        };
        let account_id = account_id.to_owned();
        cx.subscribe(&view, move |this, _, session_renewal::Renewed(name), cx| {
            if let Some(renewed) = this.row_mut(&account_id, storefront) {
                renewed.session = SessionCheck::Valid(name.clone());
            }
            this.reload_accounts(cx);
        })
        .detach();
    }

    fn set_move_error(this: &WeakEntity<Self>, message: Text, cx: &mut AsyncApp) {
        let _ = this.update(cx, |this, cx| {
            this.move_error = Some(message);
            cx.notify();
        });
    }

    /// Moves this profile's library (and whatever's already installed in
    /// it) somewhere else — a different disk, most likely, which is why the
    /// move falls back to a real copy rather than assuming a plain rename
    /// always works (see `decanter_core::Library::move_to`'s own doc).
    fn change_location(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.clone();
        let background = AppState::get(cx).background.clone();
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("settings.account.library.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            // Content goes under `.../steamapps`, matching Steam's own
            // layout for a library folder — see `Library::root_under`'s own
            // doc.
            let new_root = decanter_core::Library::root_under(folder.path());

            let Ok(db) = crate::core::db() else {
                Self::set_move_error(&this, Text::new("settings.account.errors.db"), cx);
                return;
            };
            let Ok(accounts) = db.accounts_in_profile(&profile_id) else {
                Self::set_move_error(&this, Text::new("settings.account.errors.accounts"), cx);
                return;
            };
            let Ok(library) = crate::core::library_for_profile(&profile_id) else {
                Self::set_move_error(&this, Text::new("settings.account.errors.library"), cx);
                return;
            };

            // Nothing belonging to this profile may be live during the move
            // — a running game still writing into `compatdata/`, or an
            // install still writing into `common/`, would corrupt mid-copy.
            let busy = background.read_with(cx, |background, _| {
                accounts.iter().any(|account| {
                    library
                        .scan(&account.id)
                        .keys()
                        .any(|id| background.running.contains(id) || background.installing.contains_key(id))
                })
            });
            if busy {
                Self::set_move_error(&this, Text::new("settings.account.errors.busy"), cx);
                return;
            }

            let _ = this.update(cx, |this, cx| {
                this.moving = true;
                this.move_error = None;
                this.move_progress = Some((0, 0));
                cx.notify();
            });
            tracing::info!(profile_id, to = %new_root.display(), "moving library");

            let old_path = library.root().display().to_string();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<decanter_core::MoveProgress>();
            let move_task = tokio::task::spawn_blocking(move || {
                let old_root = library.root().to_path_buf();
                // A library's database moves along with its root, whether or
                // not it's the active one, and no other decanter may start in
                // between and find it gone.
                decanter_core::bootstrap::exclusive(|| {
                    let moved = library.move_to(&new_root, move |progress| {
                        let _ = tx.send(progress);
                    })?;
                    decanter_core::bootstrap::relocate(&old_root, moved.root())?;
                    let relinked = crate::taskbar::relocate(&old_root, moved.root());
                    Ok::<_, std::io::Error>((moved, relinked))
                })
            });

            while let Some(progress) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.move_progress = Some((progress.bytes_copied, progress.total_bytes));
                    cx.notify();
                });
            }

            // Saved here, outside any view update, so the new path is
            // recorded even if Ajustes was closed mid-move — otherwise the
            // files would sit at the new root with the profile still
            // pointing at the old one.
            let error = record_move(move_task.await, &profile_id, &old_path).await;
            let _ = this.update(cx, |this, cx| {
                this.moving = false;
                this.move_progress = None;
                if error.is_none() {
                    this.reload_library_path();
                }
                this.move_error = error;
                cx.notify();
            });
        })
        .detach();
    }

    fn library_location(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let moving = self.moving;
        column()
            .child(theme::h3(t!("settings.account.library.title")))
            .child(theme::muted(
                self.library_path
                    .clone()
                    .unwrap_or_else(|| t!("settings.account.library.unknown").into_owned()),
            ))
            .children(self.move_progress.map(|(copied, total)| {
                theme::muted(t!(
                    "settings.account.library.moving_progress",
                    done = crate::format::bytes(copied),
                    total = crate::format::bytes(total)
                ))
            }))
            .children(self.move_error.as_ref().map(theme::error))
            .child(inline(
                button(
                    "move-library",
                    ButtonKind::Secondary,
                    if moving {
                        t!("settings.account.library.moving")
                    } else {
                        t!("settings.account.library.change")
                    },
                    moving,
                )
                .when(!moving, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.change_location(cx)))
                }),
            ))
    }

    /// Switches to `root` with the profile last active there, or its first
    /// one when that's unknown: a library is only ever entered through one
    /// of its profiles.
    fn switch_library(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(profiles) = self
            .libraries
            .iter()
            .find(|(known, _)| known == root)
            .and_then(|(_, profiles)| profiles.as_ref())
        else {
            return;
        };
        let db_path = decanter_core::bootstrap::db_path(root);
        let last_active = decanter_core::Db::open_read_only(&db_path)
            .and_then(|db| db.active_profile())
            .inspect_err(
                |err| tracing::warn!(path = %db_path.display(), %err, "cannot read another library's active profile"),
            )
            .ok()
            .flatten()
            .filter(|active| profiles.iter().any(|profile| profile.id == active.id))
            .map(|active| active.id);
        let Some(profile_id) = last_active.or_else(|| profiles.first().map(|profile| profile.id.clone())) else {
            self.switch_error = Some(Text::new("settings.account.library.no_profiles"));
            cx.notify();
            return;
        };
        self.switch_error = shell::switch_library(root, &profile_id, cx).err();
        cx.notify();
    }

    fn library_switcher(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.libraries.len() < 2 {
            return None;
        }
        let options = self.libraries.iter().map(|(root, profiles)| {
            let label = match profiles {
                Some(_) => root.display().to_string(),
                None => t!("chrome.profiles.unavailable", path = root.display()).into_owned(),
            };
            (
                SharedString::from(root.display().to_string()),
                SharedString::from(label),
            )
        });
        let selected = self
            .active_root
            .as_ref()
            .map(|root| root.display().to_string())
            .unwrap_or_default();
        let entity = cx.entity();
        Some(
            column()
                .child(theme::h3(t!("settings.account.library.active_title")))
                .child(theme::muted(t!("settings.account.library.active_hint")))
                .children(self.switch_error.as_ref().map(theme::error))
                .child(
                    field_row().child(Select::new("library-switcher", options, selected).on_change(
                        move |chosen, _, cx| {
                            let root = PathBuf::from(chosen.to_string());
                            entity.update(cx, |this, cx| {
                                if this.active_root.as_ref() != Some(&root) {
                                    this.switch_library(&root, cx);
                                }
                            });
                        },
                    )),
                ),
        )
    }

    fn open_new_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| ProfileForm::new(profile_form::Layout::Inline, window, cx));
        let subscription = cx.subscribe(&form, |this, _, _: &profile_form::Done, cx| {
            this.new_profile = None;
            cx.notify();
        });
        self.new_profile = Some((form, subscription));
        cx.notify();
    }

    fn people(&self, cx: &Context<Self>) -> impl IntoElement {
        let section = column()
            .child(theme::h3(t!("settings.account.people.title")))
            .child(theme::muted(t!("settings.account.people.hint")));
        match &self.new_profile {
            Some((form, _)) => section.child(form.clone()),
            None => section.child(inline(
                button(
                    "add-person",
                    ButtonKind::Secondary,
                    t!("settings.account.people.add"),
                    false,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_new_profile(window, cx))),
            )),
        }
    }

    fn account_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        theme::card().children(self.rows.iter().enumerate().flat_map(|(index, row)| {
            let failed = match &row.status {
                AccountStatus::Failed(message) => Some(message.clone()),
                AccountStatus::Idle | AccountStatus::Working => None,
            };
            let label = match &row.session {
                SessionCheck::Pending => theme::muted(t!("settings.account.session.checking")),
                SessionCheck::Valid(name) => div().font_weight(FontWeight::BOLD).child(name.clone()),
                SessionCheck::Closed => theme::error(t!("settings.account.session.revoked")),
                SessionCheck::Unreachable => theme::muted(t!("settings.account.session.unreachable")),
                SessionCheck::Failed(err) => theme::error(t!("settings.account.session.invalid", error = err)),
            };
            let renew_kind = match row.session {
                SessionCheck::Closed => Some(ButtonKind::Primary),
                SessionCheck::Unreachable => Some(ButtonKind::Secondary),
                SessionCheck::Pending | SessionCheck::Valid(_) | SessionCheck::Failed(_) => None,
            };
            let working = row.status == AccountStatus::Working;
            let account_id = row.account.id.clone();
            let storefront = row.account.storefront;
            let renew = renew_kind.map(|kind| {
                let account_id = account_id.clone();
                button(("renew", index), kind, t!("settings.account.renew"), working).when(!working, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.renew(&account_id, storefront, cx)))
                })
            });
            // The error line follows the row inside the card, so the row
            // only counts as `:last-child` when there's none.
            let last = index + 1 == count && failed.is_none();
            let line = theme::list_row(("account-row", index), last)
                .child(theme::muted(storefront_label(storefront)).flex_none().w(px(128.)))
                .child(div().flex_1().min_w_0().child(label))
                .children(renew)
                .child(
                    button(
                        ("log-out", index),
                        ButtonKind::Secondary,
                        t!("settings.account.log_out"),
                        working,
                    )
                    .when(!working, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.log_out(account_id.clone(), storefront, cx);
                        }))
                    }),
                )
                .into_any_element();
            std::iter::once(line).chain(failed.map(|message| theme::error(message).into_any_element()))
        }))
    }

    fn add_gog_account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.adding {
            return inline(
                button("add-gog", ButtonKind::Primary, t!("settings.account.add.button"), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.adding = true;
                        cx.notify();
                    },
                )),
            );
        }
        div().child(self.login.clone())
    }
}

/// A library move run under the libraries' lock: `None` when another decanter held it.
type LockedMove = Option<std::io::Result<(decanter_core::Library, bool)>>;

async fn record_move(
    outcome: Result<LockedMove, tokio::task::JoinError>,
    profile_id: &str,
    old_path: &str,
) -> Option<Text> {
    match outcome {
        Ok(Some(Ok((moved, relinked)))) => save_moved_path(&moved, relinked, profile_id, old_path).await,
        Ok(Some(Err(err))) => {
            tracing::error!(profile_id, %err, "failed to move library");
            Some(Text::verbatim(err))
        }
        Ok(None) => {
            tracing::error!(profile_id, "library move refused, another decanter holds the libraries");
            Some(Text::new("settings.account.errors.locked"))
        }
        Err(err) => {
            tracing::error!(profile_id, %err, "move task panicked");
            Some(Text::new("common.errors.internal_task").arg("error", err))
        }
    }
}

async fn save_moved_path(
    moved: &decanter_core::Library,
    relinked: bool,
    profile_id: &str,
    old_path: &str,
) -> Option<Text> {
    if relinked {
        crate::taskbar::refresh().await;
    }
    let new_path = moved.root().display().to_string();
    match repoint_profiles(profile_id, old_path, &new_path) {
        Ok(()) => {
            tracing::info!(profile_id, "library moved");
            None
        }
        Err(err) => {
            tracing::error!(profile_id, %err, "library moved but failed to save new path");
            Some(Text::new("settings.account.errors.save_path").arg("error", err))
        }
    }
}

fn repoint_profiles(profile_id: &str, old_path: &str, new_path: &str) -> Result<(), crate::core::StoreError> {
    let moved_db = crate::achv_try!(crate::core::db());
    let profiles = crate::achv_try!(moved_db.list_profiles().map_err(crate::core::StoreError::Query));
    crate::achv_try!(
        moved_db
            .rebase_library_caches(std::path::Path::new(old_path), std::path::Path::new(new_path))
            .map_err(crate::core::StoreError::Query)
    );
    crate::achv_try!(
        profiles
            .iter()
            .filter(|profile| profile.id == profile_id || profile.library_path.as_deref() == Some(old_path))
            .try_for_each(|profile| {
                moved_db
                    .set_profile_library_path(&profile.id, Some(new_path))
                    .map_err(crate::core::StoreError::Query)
            })
    );
    crate::core::forget_redundant_library_paths();
    Ok(())
}

impl Render for AccountTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = if self.rows.is_empty() {
            theme::empty(t!("settings.account.empty")).into_any_element()
        } else {
            self.account_rows(cx).into_any_element()
        };
        column()
            .child(self.library_location(cx))
            .children(self.library_switcher(cx))
            .child(theme::muted(t!("settings.account.intro")))
            .child(accounts)
            .child(self.add_gog_account(cx))
            .child(self.people(cx))
    }
}
