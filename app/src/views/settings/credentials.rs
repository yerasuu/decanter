//! Saved credentials in Ajustes › General: what decanter keeps in the system keyring, by kind and
//! subject and never by value, each forgotten after asking, and whether the keyring answers
//! (design: `docs/secure-credential-storage-design.md`, 3.6 and 3.7).

use decanter_core::Storefront;
use decanter_core::secrets::{self, SecretKey, SecretKind, Status};
use gpui::{Context, IntoElement, Window, div, prelude::*};
use rust_i18n::t;

use super::{column, field_row, inline};
use crate::theme;
use crate::widgets::{ButtonKind, button, confirm_panel};

struct Row {
    key: SecretKey,
    /// The GOG account's name, when a profile still has that account.
    account_name: Option<String>,
    /// A session whose account no profile has: left behind, forgettable.
    orphan: bool,
}

enum Load {
    Loading,
    Ready(Vec<Row>),
    /// The keyring could not be listed; nothing is inferred about what it holds.
    Failed,
}

enum Confirm {
    One(SecretKey),
    All,
}

pub struct CredentialsSection {
    load: Load,
    confirm: Option<Confirm>,
    busy: bool,
    notice: Option<String>,
}

impl CredentialsSection {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            load: Load::Loading,
            confirm: None,
            busy: false,
            notice: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let rows = cx.background_spawn(async { load_rows() }).await;
            let _ = this.update(cx, |this, cx| {
                this.load = rows.map_or(Load::Failed, Load::Ready);
                cx.notify();
            });
        })
        .detach();
    }

    fn forget(&mut self, key: SecretKey, cx: &mut Context<Self>) {
        self.confirm = None;
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { secrets::store().delete(&key) }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.notice = Some(match result {
                    Ok(()) => t!("settings.credentials.forgotten").into_owned(),
                    Err(err) => t!("settings.credentials.errors.forget", error = err).into_owned(),
                });
                this.reload(cx);
            });
        })
        .detach();
    }

    fn forget_all(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async { secrets::store().forget_all(&known_keys(&accounts())) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.notice = Some(match result {
                    Ok(done) if done.failed == 0 => {
                        t!("settings.credentials.forgotten_all", count = done.removed).into_owned()
                    }
                    Ok(done) => t!(
                        "settings.credentials.forgotten_some",
                        count = done.removed,
                        failed = done.failed
                    )
                    .into_owned(),
                    Err(err) => t!("settings.credentials.errors.forget", error = err).into_owned(),
                });
                this.reload(cx);
            });
        })
        .detach();
    }

    fn status_line(&self) -> String {
        match secrets::store().status() {
            Status::Ready(_) => t!("settings.credentials.status.ready").into_owned(),
            Status::Locked { reason } => t!("settings.credentials.status.locked", reason = reason).into_owned(),
            Status::Missing { reason } => t!("settings.credentials.status.missing", reason = reason).into_owned(),
            _ => t!("settings.credentials.status.unknown").into_owned(),
        }
    }

    fn confirm_panel(&self, confirm: &Confirm, rows: &[Row], cx: &Context<Self>) -> impl IntoElement + use<> {
        let entity = cx.entity();
        let no = {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                entity.update(cx, |this, cx| {
                    this.confirm = None;
                    cx.notify();
                });
            }
        };
        match confirm {
            Confirm::One(key) => {
                let subject = rows
                    .iter()
                    .find(|row| row.key == *key)
                    .map_or_else(|| key.subject().unwrap_or_default().to_owned(), subject_label);
                let key = key.clone();
                let body = match key.kind() {
                    SecretKind::GogSession => t!("settings.credentials.confirm.gog_session", subject = subject),
                    SecretKind::ShadnetPassword => {
                        t!("settings.credentials.confirm.shadnet_password", subject = subject)
                    }
                    SecretKind::ShadnetToken => t!("settings.credentials.confirm.shadnet_token", subject = subject),
                    _ => t!("settings.credentials.confirm.steamgriddb_key"),
                };
                confirm_panel(
                    "cred-confirm-one",
                    t!("settings.credentials.confirm.title"),
                    body,
                    t!("settings.credentials.forget"),
                    move |_, _, cx| {
                        let key = key.clone();
                        entity.update(cx, |this, cx| this.forget(key, cx));
                    },
                    no,
                )
            }
            Confirm::All => confirm_panel(
                "cred-confirm-all",
                t!("settings.credentials.confirm.title"),
                t!("settings.credentials.confirm.all", count = rows.len()),
                t!("settings.credentials.forget_all"),
                move |_, _, cx| entity.update(cx, |this, cx| this.forget_all(cx)),
                no,
            ),
        }
    }
}

impl Render for CredentialsSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = matches!(secrets::store().status(), Status::Ready(_));
        let rows: &[Row] = match &self.load {
            Load::Ready(rows) => rows,
            Load::Loading | Load::Failed => &[],
        };
        let list = match &self.load {
            Load::Loading => theme::muted(t!("settings.credentials.loading")).into_any_element(),
            Load::Failed => theme::error(t!("settings.credentials.unavailable")).into_any_element(),
            Load::Ready(rows) if rows.is_empty() => theme::empty(t!("settings.credentials.empty")).into_any_element(),
            Load::Ready(rows) => theme::card()
                .max_w(gpui::px(640.))
                .children(rows.iter().enumerate().map(|(index, row)| {
                    let key = row.key.clone();
                    theme::compact_row(index + 1 == rows.len())
                        .child(
                            column()
                                .flex_1()
                                .min_w_0()
                                .child(kind_label(row.key.kind()))
                                .child(theme::muted(subject_label(row)))
                                .when(row.orphan, |col| {
                                    col.child(theme::muted(t!("settings.credentials.orphan")))
                                }),
                        )
                        .child(
                            button(
                                ("cred-forget", index),
                                ButtonKind::Secondary,
                                t!("settings.credentials.forget"),
                                self.busy,
                            )
                            .when(!self.busy, |button| {
                                button.on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirm = Some(Confirm::One(key.clone()));
                                    cx.notify();
                                }))
                            }),
                        )
                }))
                .into_any_element(),
        };
        let confirm = self
            .confirm
            .as_ref()
            .map(|confirm| self.confirm_panel(confirm, rows, cx));
        column()
            .child(theme::h2(t!("settings.credentials.title")))
            .child(theme::muted(t!("settings.credentials.hint")))
            .child(
                field_row()
                    .mt_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(self.status_line()))
                    .when(!ready, |row| {
                        row.child(
                            button(
                                "cred-retry",
                                ButtonKind::Secondary,
                                t!("settings.credentials.retry"),
                                matches!(self.load, Load::Loading),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        )
                    }),
            )
            .child(div().mb_2().child(list))
            .children(confirm.map(|panel| div().mb_2().max_w(gpui::px(640.)).child(panel)))
            .when(!rows.is_empty() && self.confirm.is_none(), |column| {
                column.child(inline(
                    button(
                        "cred-forget-all",
                        ButtonKind::Secondary,
                        t!("settings.credentials.forget_all"),
                        self.busy,
                    )
                    .when(!self.busy, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| {
                            this.confirm = Some(Confirm::All);
                            cx.notify();
                        }))
                    }),
                ))
            })
            .children(self.notice.as_ref().map(|notice| theme::muted(notice.clone()).mt_2()))
    }
}

fn kind_label(kind: SecretKind) -> String {
    match kind {
        SecretKind::GogSession => t!("settings.credentials.kind.gog_session"),
        SecretKind::ShadnetPassword => t!("settings.credentials.kind.shadnet_password"),
        SecretKind::ShadnetToken => t!("settings.credentials.kind.shadnet_token"),
        _ => t!("settings.credentials.kind.steamgriddb_key"),
    }
    .into_owned()
}

fn subject_label(row: &Row) -> String {
    match (&row.account_name, row.key.subject()) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (None, Some(subject)) => subject.to_owned(),
        (_, None) => t!("settings.credentials.subject.app").into_owned(),
    }
}

/// Every account of every profile; a database that will not open lists none.
fn accounts() -> Vec<decanter_core::Account> {
    let Ok(db) = crate::core::db() else {
        return Vec::new();
    };
    let profiles = db
        .list_profiles()
        .inspect_err(|err| tracing::warn!(%err, "could not list profiles for the credentials list"))
        .unwrap_or_default();
    profiles
        .iter()
        .flat_map(|profile| db.accounts_in_profile(&profile.id).unwrap_or_default())
        .collect()
}

/// The entries decanter has a name for, asked one by one when the keyring cannot search.
fn known_keys(accounts: &[decanter_core::Account]) -> Vec<SecretKey> {
    accounts
        .iter()
        .filter(|account| account.storefront == Storefront::Gog)
        .filter_map(|account| SecretKey::gog_session(&account.id).ok())
        .chain([SecretKey::STEAMGRIDDB])
        .collect()
}

fn load_rows() -> Result<Vec<Row>, decanter_core::secrets::StoreError> {
    let accounts = accounts();
    let listing = secrets::store().list(&known_keys(&accounts))?;
    if listing.unrecognized > 0 {
        tracing::warn!(
            count = listing.unrecognized,
            "keyring entries of decanter's service left unlisted"
        );
    }
    Ok(listing
        .keys
        .into_iter()
        .map(|key| {
            let account = (key.kind() == SecretKind::GogSession)
                .then(|| {
                    accounts.iter().find(|account| {
                        account.storefront == Storefront::Gog && Some(account.id.as_str()) == key.subject()
                    })
                })
                .flatten();
            Row {
                orphan: key.kind() == SecretKind::GogSession && account.is_none(),
                account_name: account.map(|account| account.name.clone()),
                key,
            }
        })
        .collect())
}
