//! The profile's storefront accounts — decanter holds every session itself
//! now, so this is where one gets added or dropped, no next-ui, no
//! plugin. A profile can hold several, even several of the same
//! storefront: each shows its own row, with its own "Cerrar sesión".
//!
//! Also where the profile's library lives, since moving it is a
//! per-profile concern too.

use decanter_core::{Account, Storefront};
use gpui::{AsyncApp, Context, Entity, FontWeight, IntoElement, WeakEntity, Window, div, prelude::*, px};
use rust_i18n::t;

use super::{column, field_row, inline, storefront_label};
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

#[derive(Clone, PartialEq)]
enum AccountStatus {
    Idle,
    Working,
    Failed(String),
}

/// The upstream session check behind a row's label.
enum SessionCheck {
    Pending,
    Valid(String),
    Revoked,
    Failed(String),
}

struct AccountRow {
    account: Account,
    session: SessionCheck,
    status: AccountStatus,
}

pub struct AccountTab {
    profile_id: String,
    library_path: String,
    moving: bool,
    move_progress: Option<(u64, u64)>,
    move_error: Option<String>,
    rows: Vec<AccountRow>,
    adding: bool,
    pasted: Entity<TextInput>,
    add_status: AccountStatus,
}

impl AccountTab {
    pub fn new(profile_id: String, cx: &mut Context<Self>) -> Self {
        let pasted = cx.new(|cx| TextInput::new("https://embed.gog.com/on_login_success?…&code=…", cx));
        cx.subscribe(&pasted, |_, _, event, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let mut this = Self {
            profile_id,
            library_path: String::new(),
            moving: false,
            move_progress: None,
            move_error: None,
            rows: Vec::new(),
            adding: false,
            pasted,
            add_status: AccountStatus::Idle,
        };
        this.reload_library_path();
        this.reload_accounts(cx);
        this
    }

    fn reload_library_path(&mut self) {
        self.library_path = crate::core::library_for_profile(&self.profile_id)
            .map(|library| library.root().display().to_string())
            .unwrap_or_else(|_| t!("settings.account.library.unknown").into_owned());
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
    /// revoked shows as such instead of a dead "connected" state.
    fn check_session(account: &Account, cx: &mut Context<Self>) {
        let account_id = account.id.clone();
        let storefront = account.storefront;
        cx.spawn(async move |this, cx| {
            tracing::debug!(account_id, "checking gog session status");
            let check = match decanter_gog::session(&account_id).await {
                Ok(Some(session)) => SessionCheck::Valid(session.display_name),
                Ok(None) => {
                    // Rendered as a red "la sesión se cerró en GOG" —
                    // visible to the user, not just a cache miss.
                    tracing::error!(account_id, "gog session no longer valid, revoked upstream");
                    SessionCheck::Revoked
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
                    AccountStatus::Failed(err.to_string()),
                    cx,
                );
                return;
            }
            match crate::core::db().and_then(|db| {
                db.forget_account(&account_id, storefront)
                    .map_err(|err| err.to_string())
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
                    Self::set_row_status(&this, &account_id, storefront, AccountStatus::Failed(err), cx);
                }
            }
        })
        .detach();
    }

    fn open_gog(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::desktop::open(decanter_gog::auth_url()) {
            tracing::error!(error = %err, "failed to open browser for gog login");
            self.add_status = AccountStatus::Failed(t!("settings.account.errors.browser", error = err).into_owned());
            cx.notify();
        }
    }

    /// GOG's OAuth has no loopback or device flow: the only way through is
    /// a browser login that lands on a page whose URL carries a one-shot
    /// code. So the user opens the page, logs in, and pastes the URL they
    /// end up on back here. No account id goes into `decanter_gog::log_in`
    /// — which account this becomes is only known once GOG answers, so
    /// `profile_id` (already known) is what ties the *result* to this
    /// profile, via `upsert_account`.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.clone();
        let code = self.pasted.read(cx).text().to_owned();
        self.add_status = AccountStatus::Working;
        cx.notify();
        tracing::info!(profile_id, "logging in to gog");
        let library = AppState::get(cx).library.clone();
        cx.spawn(async move |this, cx| {
            let outcome = match decanter_gog::log_in(&code).await {
                Ok(account) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_secs() as i64);
                    crate::core::db()
                        .and_then(|db| {
                            db.upsert_account(
                                &account.account_id,
                                Storefront::Gog,
                                &profile_id,
                                &account.display_name,
                                now,
                            )
                            .map_err(|err| err.to_string())
                        })
                        .map(|_| account.account_id)
                        .inspect_err(|err| tracing::error!(profile_id, error = %err, "failed to save gog account"))
                }
                Err(err) => {
                    tracing::error!(profile_id, error = %err, "gog login failed");
                    Err(err.to_string())
                }
            };
            match outcome {
                Ok(account_id) => {
                    tracing::info!(profile_id, account_id, "gog account added");
                    library.update(cx, |library, cx| library.refresh(cx));
                    let _ = this.update(cx, |this, cx| {
                        this.pasted.update(cx, |input, cx| input.set_text("", cx));
                        this.add_status = AccountStatus::Idle;
                        this.adding = false;
                        this.reload_accounts(cx);
                    });
                }
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.add_status = AccountStatus::Failed(err);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn set_move_error(this: &WeakEntity<Self>, message: String, cx: &mut AsyncApp) {
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
                Self::set_move_error(&this, t!("settings.account.errors.db").into_owned(), cx);
                return;
            };
            let Ok(accounts) = db.accounts_in_profile(&profile_id) else {
                Self::set_move_error(&this, t!("settings.account.errors.accounts").into_owned(), cx);
                return;
            };
            let Ok(library) = crate::core::library_for_profile(&profile_id) else {
                Self::set_move_error(&this, t!("settings.account.errors.library").into_owned(), cx);
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
                Self::set_move_error(&this, t!("settings.account.errors.busy").into_owned(), cx);
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
                .ok_or_else(|| std::io::Error::other(t!("settings.account.errors.locked").into_owned()))?
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
            let error = match move_task.await {
                Ok(Ok((moved, relinked))) => {
                    if relinked {
                        crate::taskbar::refresh().await;
                    }
                    let new_path = moved.root().display().to_string();
                    match crate::core::db().and_then(|moved_db| {
                        let profiles = moved_db.list_profiles().map_err(|err| err.to_string())?;
                        profiles
                            .iter()
                            .filter(|profile| {
                                profile.id == profile_id || profile.library_path.as_ref() == Some(&old_path)
                            })
                            .try_for_each(|profile| {
                                moved_db
                                    .set_profile_library_path(&profile.id, Some(&new_path))
                                    .map_err(|err| err.to_string())
                            })
                    }) {
                        Ok(()) => {
                            tracing::info!(profile_id, "library moved");
                            None
                        }
                        Err(err) => {
                            tracing::error!(profile_id, %err, "library moved but failed to save new path");
                            Some(t!("settings.account.errors.save_path", error = err).into_owned())
                        }
                    }
                }
                Ok(Err(err)) => {
                    tracing::error!(profile_id, %err, "failed to move library");
                    Some(err.to_string())
                }
                Err(err) => {
                    tracing::error!(profile_id, %err, "move task panicked");
                    Some(t!("common.errors.internal_task", error = err).into_owned())
                }
            };
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
            .child(theme::muted(self.library_path.clone()))
            .children(self.move_progress.map(|(copied, total)| {
                theme::muted(t!(
                    "settings.account.library.moving_progress",
                    done = crate::format::bytes(copied),
                    total = crate::format::bytes(total)
                ))
            }))
            .children(self.move_error.clone().map(theme::error))
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
                SessionCheck::Revoked => theme::error(t!("settings.account.session.revoked")),
                SessionCheck::Failed(err) => theme::error(t!("settings.account.session.invalid", error = err)),
            };
            let working = row.status == AccountStatus::Working;
            let account_id = row.account.id.clone();
            let storefront = row.account.storefront;
            // The error line follows the row inside the card, so the row
            // only counts as `:last-child` when there's none.
            let last = index + 1 == count && failed.is_none();
            let line = theme::list_row(("account-row", index), last)
                .child(theme::muted(storefront_label(storefront)).flex_none().w(px(128.)))
                .child(div().flex_1().min_w_0().child(label))
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
        let disabled = self.add_status == AccountStatus::Working || self.pasted.read(cx).text().trim().is_empty();
        // `.card` has no padding of its own; children keep their natural
        // width instead of stretching, as inline buttons did in a block.
        theme::card()
            .items_start()
            .child(theme::muted(t!("settings.account.add.hint")).w_full())
            .child(
                button("open-gog", ButtonKind::Primary, t!("settings.account.add.open"), false)
                    .on_click(cx.listener(|this, _, _, cx| this.open_gog(cx))),
            )
            .child(
                field_row()
                    .w_full()
                    .child(div().flex_1().min_w_0().child(self.pasted.clone()))
                    .child(
                        button(
                            "connect-gog",
                            ButtonKind::Primary,
                            t!("settings.account.add.connect"),
                            disabled,
                        )
                        .when(!disabled, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                        }),
                    ),
            )
            .children(match &self.add_status {
                AccountStatus::Failed(message) => Some(theme::error(message.clone()).w_full()),
                AccountStatus::Idle | AccountStatus::Working => None,
            })
    }
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
            .child(theme::muted(t!("settings.account.intro")))
            .child(accounts)
            .child(self.add_gog_account(cx))
    }
}
