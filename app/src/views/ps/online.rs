//! Ajustes' online group: the shadNet account and its servers, which every game shares; whether a
//! game plays online is its own Online tab's. The password and token go to the keyring, never to
//! the library (decision #715); the connection test runs only when clicked (decision #601).

use decanter_core::capabilities::Feature;
use decanter_core::secrets::{self, Secret, SecretKey, ShadnetSecret};
use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::online::{self, Addresses, Probe, Target};
use gpui::{AppContext as _, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::state::online_error;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button};

/// The text fields' settings keys, in the order they are shown.
const FIELDS: [(&str, &str); 4] = [
    (online::SERVER, "ps.online.server"),
    (online::WEBAPI, "ps.online.webapi"),
    (online::COMMUNITY, "ps.online.community"),
    (online::NPID, "ps.online.npid"),
];

/// What the keyring holds for the saved NPID.
#[derive(Clone, Copy, Default)]
struct Stored {
    password: bool,
    token: bool,
}

pub struct OnlineView {
    fields: [Entity<TextInput>; 4],
    password: Entity<TextInput>,
    token: Entity<TextInput>,
    /// The NPID the settings file holds, whose secrets [`Self::stored`] describes.
    saved_npid: Option<String>,
    stored: Result<Stored, String>,
    /// Also while the first read is under way, since saving needs [`Self::saved_npid`].
    is_busy: bool,
    message: Option<(Text, bool)>,
    probes: Option<[Probe; 3]>,
}

impl OnlineView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let fields = [
            cx.new(|cx| TextInput::new(online::SHADNET_SERVER, cx)),
            cx.new(|cx| TextInput::new(Text::new("ps.online.webapi_placeholder"), cx)),
            cx.new(|cx| TextInput::new(online::COMMUNITY_SERVER, cx)),
            cx.new(|cx| TextInput::new(Text::new("ps.online.npid_placeholder"), cx)),
        ];
        let this = Self {
            fields,
            password: cx.new(|cx| TextInput::new("", cx).masked()),
            token: cx.new(|cx| TextInput::new(Text::new("ps.online.token_placeholder"), cx).masked()),
            saved_npid: None,
            stored: Ok(Stored::default()),
            is_busy: true,
            message: None,
            probes: None,
        };
        Self::reload(cx);
        this
    }

    fn reload(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let settings = cx
                .background_spawn(async { LauncherSettings::load(&decanter_ps::paths::active()) })
                .await;
            let npid = settings
                .text(online::NPID)
                .and_then(|npid| online::check_npid(npid).ok().flatten())
                .map(str::to_owned);
            let stored = stored_secrets(npid.clone()).await;
            let _ = this.update(cx, |this, cx| {
                for (field, (key, _)) in this.fields.iter().zip(FIELDS) {
                    let value = settings.text(key).unwrap_or_default().to_owned();
                    field.update(cx, |field, cx| field.set_text(value, cx));
                }
                this.saved_npid = npid;
                this.stored = stored;
                this.is_busy = false;
                this.refresh_password_placeholder(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_password_placeholder(&self, cx: &mut Context<Self>) {
        let placeholder = match &self.stored {
            Ok(stored) if stored.password => Text::new("ps.online.password_saved"),
            Ok(_) => Text::new("ps.online.password_none"),
            Err(_) => Text::new("ps.online.password_unavailable"),
        };
        self.password
            .update(cx, |field, cx| field.set_placeholder(placeholder, cx));
    }

    fn text(&self, index: usize, cx: &Context<Self>) -> String {
        self.fields
            .get(index)
            .map(|field| field.read(cx).text().trim().to_owned())
            .unwrap_or_default()
    }

    fn addresses(&self, cx: &Context<Self>) -> Result<Addresses, Text> {
        Addresses::new(&self.text(0, cx), &self.text(1, cx), &self.text(2, cx)).map_err(|err| online_error(&err))
    }

    /// The fields to the settings file, typed secrets to the keyring under the NPID; the
    /// secrets of an NPID given up are forgotten.
    fn save(&mut self, cx: &mut Context<Self>) {
        let checked = self.addresses(cx).and_then(|_| {
            online::check_npid(&self.text(3, cx))
                .map(|npid| npid.map(str::to_owned))
                .map_err(|err| online_error(&err))
        });
        let npid = match checked {
            Ok(npid) => npid,
            Err(err) => {
                self.message = Some((err, true));
                cx.notify();
                return;
            }
        };
        let password = self.password.read(cx).text().to_owned();
        let token = self.token.read(cx).text().to_owned();
        if npid.is_none() && !(password.is_empty() && token.is_empty()) {
            self.message = Some((Text::new("ps.online.errors.secret_without_npid"), true));
            cx.notify();
            return;
        }
        let values: Vec<_> = FIELDS
            .iter()
            .enumerate()
            .map(|(index, (key, _))| (*key, self.text(index, cx)))
            .collect();
        let previous = self
            .saved_npid
            .clone()
            .filter(|previous| npid.as_ref() != Some(previous));
        self.is_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            // The secrets first: a settings file naming an NPID whose secrets didn't make it to the
            // keyring would launch without them. The NPID given up keeps its own until the file
            // names the new one.
            let copy_npid = npid.clone();
            let copy_previous = previous.clone();
            let copied = tokio::task::spawn_blocking(move || {
                copy_secrets(copy_npid.as_deref(), copy_previous.as_deref(), password, token)
            })
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
            let moved = match copied {
                Ok(moved) => moved,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.is_busy = false;
                        this.message = Some((Text::new("ps.online.keyring_error").arg("error", err), true));
                        cx.notify();
                    });
                    return;
                }
            };
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit(&decanter_ps::paths::active(), |settings| {
                        for (key, value) in &values {
                            if value.is_empty() {
                                settings.remove(key);
                            } else {
                                settings.set_text(key, value);
                            }
                        }
                    })
                })
                .await;
            let outcome = match (saved, previous) {
                (Err(err), _) => Err(Text::new("ps.online.save_error").arg("error", err)),
                (Ok(()), Some(previous)) if moved == Moved::All => {
                    tokio::task::spawn_blocking(move || forget(&previous))
                        .await
                        .unwrap_or_else(|err| Err(err.to_string()))
                        .map(|()| Text::new("ps.online.saved"))
                        .map_err(|err| Text::new("ps.online.keyring_error").arg("error", err))
                }
                (Ok(()), Some(previous)) => Ok(Text::new("ps.online.kept_existing").arg("previous", previous)),
                (Ok(()), None) => Ok(Text::new("ps.online.saved")),
            };
            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                this.message = Some(match outcome {
                    Ok(text) => {
                        this.password.update(cx, |field, cx| field.set_text("", cx));
                        this.token.update(cx, |field, cx| field.set_text("", cx));
                        (text, false)
                    }
                    Err(err) => (err, true),
                });
                Self::reload(cx);
            });
        })
        .detach();
    }

    fn forget_secrets(&mut self, cx: &mut Context<Self>) {
        let Some(npid) = self.saved_npid.clone() else {
            return;
        };
        self.is_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let forgotten = tokio::task::spawn_blocking(move || forget(&npid))
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                this.message = forgotten
                    .err()
                    .map(|err| (Text::new("ps.online.keyring_error").arg("error", err), true));
                Self::reload(cx);
            });
        })
        .detach();
    }

    /// Only ever from the button: nothing connects on its own (decision #601).
    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let addresses = match self.addresses(cx) {
            Ok(addresses) => addresses,
            Err(err) => {
                self.message = Some((err, true));
                cx.notify();
                return;
            }
        };
        self.is_busy = true;
        self.probes = None;
        self.message = Some((Text::new("ps.online.checking"), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let probes = online::check(&addresses).await;
            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                this.message = None;
                this.probes = Some(probes);
                cx.notify();
            });
        })
        .detach();
    }

    fn field_row(label: SharedString, field: Entity<TextInput>, last: bool) -> gpui::Div {
        theme::compact_row(last)
            .child(div().w(px(180.)).flex_none().child(label))
            .child(div().flex_1().min_w_0().child(field))
    }

    fn probe_row(probe: &Probe, last: bool) -> gpui::Div {
        let name = match probe.target {
            Target::Game => t!("ps.online.targets.game"),
            Target::Shadnet => t!("ps.online.targets.shadnet"),
            Target::Webapi => t!("ps.online.targets.webapi"),
        };
        let (mark, color, detail) = match &probe.outcome {
            Ok(()) => ("✓", theme::ACCENT_GREEN, t!("ps.online.answers").into_owned()),
            Err(err) => ("✗", theme::ERROR_TEXT, err.clone()),
        };
        theme::compact_row(last)
            .child(div().w_4().flex_none().text_color(rgb(color)).child(mark))
            .child(div().w(px(160.)).flex_none().child(SharedString::from(name)))
            .child(theme::mono().flex_1().min_w_0().truncate().child(probe.address.clone()))
            .child(theme::muted(detail).max_w(px(300.)).truncate())
    }
}

impl Render for OnlineView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caps = Caps::get(cx);
        let has_saved = matches!(self.stored, Ok(stored) if stored.password || stored.token);
        let keyring_error = self.stored.as_ref().err().cloned();
        let lacks_password = self.saved_npid.is_some() && matches!(self.stored, Ok(stored) if !stored.password);
        let mut fields = div().flex().flex_col();
        for (field, (_, label)) in self.fields.iter().zip(FIELDS) {
            fields = fields.child(Self::field_row(t!(label).into(), field.clone(), false));
        }
        let account = fields
            .child(Self::field_row(
                t!("ps.online.password").into(),
                self.password.clone(),
                false,
            ))
            .child(Self::field_row(t!("ps.online.token").into(), self.token.clone(), true));
        let busy = self.is_busy;
        let actions = div()
            .flex()
            .gap_2()
            .child(
                button("ps-online-save", ButtonKind::Primary, t!("ps.online.save"), busy)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
            .child(
                button("ps-online-check", ButtonKind::Secondary, t!("ps.online.check"), busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
            )
            .when(has_saved, |actions| {
                actions.child(
                    button("ps-online-forget", ButtonKind::Secondary, t!("ps.online.forget"), busy)
                        .on_click(cx.listener(|this, _, _, cx| this.forget_secrets(cx))),
                )
            });
        let probes = self.probes.as_ref().map(|probes| {
            theme::card().children(
                probes
                    .iter()
                    .enumerate()
                    .map(|(index, probe)| Self::probe_row(probe, index + 1 == probes.len())),
            )
        });
        let is_offered = caps.is_offered(Feature::PsOnline);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(theme::h2(t!("ps.online.title")))
            .children(caps.reason(Feature::PsOnline).map(theme::muted))
            .when(is_offered, |column| {
                column
                    .child(theme::muted(t!("ps.online.per_game")))
                    .child(theme::card().child(account))
                    .child(theme::muted(t!("ps.online.account_description")))
                    .when(lacks_password, |column| {
                        column.child(theme::error(t!("ps.online.no_password")))
                    })
                    .children(keyring_error.map(|err| theme::error(t!("ps.online.keyring_unavailable", error = err))))
                    .child(actions)
                    .children(self.message.as_ref().map(|(text, is_error)| {
                        if *is_error {
                            theme::error(text)
                        } else {
                            theme::muted(text)
                        }
                    }))
                    .children(probes)
            })
    }
}

/// Whether the keyring holds a password and a token for `npid`. Blocking underneath.
async fn stored_secrets(npid: Option<String>) -> Result<Stored, String> {
    let Some(npid) = npid else {
        return Ok(Stored::default());
    };
    tokio::task::spawn_blocking(move || {
        let password = load(&npid, ShadnetSecret::Password)?;
        let token = load(&npid, ShadnetSecret::Token)?;
        Ok(Stored {
            password: password.is_some(),
            token: token.is_some(),
        })
    })
    .await
    .unwrap_or_else(|err| Err(err.to_string()))
}

/// Whether everything the NPID given up had now lives under the new one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moved {
    All,
    /// The new NPID had a secret of its own, kept, so the old one keeps its own too.
    KeptBoth,
}

/// Typed secrets under `npid`. An empty field keeps what `npid` has, or else takes what
/// `previous`, the NPID given up, had; never overwrites one `npid` already has. Blocking: keyring
/// round trips.
fn copy_secrets(npid: Option<&str>, previous: Option<&str>, password: String, token: String) -> Result<Moved, String> {
    let mut moved = Moved::All;
    let Some(npid) = npid else {
        return Ok(moved);
    };
    for (secret, typed) in [(ShadnetSecret::Password, password), (ShadnetSecret::Token, token)] {
        let value = match previous {
            _ if !typed.is_empty() => Some(Secret::from(typed)),
            Some(previous) => match load(previous, secret)? {
                Some(_) if load(npid, secret)?.is_some() => {
                    moved = Moved::KeptBoth;
                    None
                }
                found => found,
            },
            None => None,
        };
        if let Some(value) = value {
            secrets::store()
                .put(&key(npid, secret)?, &value)
                .map_err(|err| err.to_string())?;
        }
    }
    Ok(moved)
}

/// Blocking: keyring round trips.
fn forget(npid: &str) -> Result<(), String> {
    for secret in [ShadnetSecret::Password, ShadnetSecret::Token] {
        secrets::store()
            .delete(&key(npid, secret)?)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn key(npid: &str, secret: ShadnetSecret) -> Result<SecretKey, String> {
    SecretKey::shadnet(npid, secret).map_err(|err| err.to_string())
}

/// Blocking: a keyring round trip.
fn load(npid: &str, secret: ShadnetSecret) -> Result<Option<Secret>, String> {
    secrets::store().get(&key(npid, secret)?).map_err(|err| err.to_string())
}
