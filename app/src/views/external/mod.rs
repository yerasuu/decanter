//! "Juegos externos": the page behind Biblioteca's "+ Agregar un juego",
//! laid out like Ajustes — tabs for adding (or editing) one, the profile's
//! list of them, and where the art search gets its SteamGridDB key. Off the
//! topbar's nav, the same as Ajustes: reached from the sidebar's button or
//! a game's right-click "Editar".

mod form;

#[cfg(proton)]
use std::collections::{HashMap, HashSet};
#[cfg(proton)]
use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_core::capabilities::Feature;
#[cfg(proton)]
use gpui::Task;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, ObjectFit, Subscription, Window, div, img, prelude::*, px, rgb,
};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::external::art;
#[cfg(proton)]
use crate::external::launchers::Detected;
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button, tooltip};
use form::{FormEvent, GameForm};

/// Asks the shell to show this page, on a fresh form or on `editing`'s id.
pub fn open(editing: Option<String>, cx: &mut App) {
    let session = AppState::get(cx).session.clone();
    session.update(cx, |session, cx| {
        session.external_request = session.external_request.wrapping_add(1);
        session.external_edit = editing;
        cx.notify();
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Form,
    Games,
    Sources,
}

pub struct ExternalView {
    tab: Tab,
    profile_id: Option<String>,
    games: Vec<InstalledGame>,
    form: Option<(Entity<GameForm>, Subscription)>,
    /// Games a launcher installed in one of `games`' prefixes that aren't
    /// in the library yet, by the id of the game owning that prefix.
    #[cfg(proton)]
    found: HashMap<String, Vec<Detected>>,
    #[cfg(proton)]
    _scan: Option<Task<()>>,
    api_key: Entity<TextInput>,
    key_status: String,
}

impl ExternalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Nothing can resume a copy from a previous session's form.
        #[cfg(proton)]
        if let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.as_deref() {
            crate::external::import::abandon_all(profile_id);
        }
        let state = AppState::get(cx).clone();
        // `running` locks a running game's Eliminar.
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |this, session, cx| {
            let profile = session.read(cx).active_profile.clone();
            if profile != this.profile_id {
                this.start(None, cx);
            }
        })
        .detach();
        let api_key = cx.new(|cx| TextInput::new(t!("external.sources.key_placeholder"), cx));
        // A keyring round trip: off the UI thread, filled in when it lands.
        let field = api_key.clone();
        cx.spawn(async move |_, cx| {
            let key = tokio::task::spawn_blocking(art::api_key).await.ok().flatten();
            if let Some(key) = key {
                field.update(cx, |input, cx| input.set_text(key, cx));
            }
        })
        .detach();
        let mut this = Self {
            tab: Tab::Form,
            profile_id: None,
            games: Vec::new(),
            form: None,
            #[cfg(proton)]
            found: HashMap::new(),
            #[cfg(proton)]
            _scan: None,
            api_key,
            key_status: String::new(),
        };
        this.start(None, cx);
        this
    }

    /// Swaps in a fresh form for the active profile — empty, or on the
    /// external game `editing` names — and shows it.
    pub fn start(&mut self, editing: Option<String>, cx: &mut Context<Self>) {
        self.profile_id = AppState::get(cx).session.read(cx).active_profile.clone();
        self.games = self
            .profile_id
            .as_deref()
            .map(crate::external::list)
            .unwrap_or_default();
        #[cfg(proton)]
        self.scan(cx);
        let editing = editing.and_then(|id| self.games.iter().find(|game| game.game_id == id).cloned());
        self.form = self
            .profile_id
            .clone()
            .map(|profile_id| Self::watch(cx.new(|cx| GameForm::new(profile_id, editing, cx)), cx));
        self.tab = Tab::Form;
        cx.notify();
    }

    /// A fresh form for a game a launcher installed in `host_id`'s prefix.
    #[cfg(proton)]
    fn start_hosted(&mut self, host_id: &str, found: Detected, cx: &mut Context<Self>) {
        let (Some(profile_id), Some(host)) = (
            self.profile_id.clone(),
            self.games.iter().find(|game| game.game_id == host_id).cloned(),
        ) else {
            return;
        };
        self.form = Some(Self::watch(
            cx.new(|cx| GameForm::hosted(profile_id, &host, &found, cx)),
            cx,
        ));
        self.tab = Tab::Form;
        cx.notify();
    }

    fn watch(form: Entity<GameForm>, cx: &mut Context<Self>) -> (Entity<GameForm>, Subscription) {
        let subscription = cx.subscribe(&form, |this, _, event, cx| match event {
            FormEvent::Saved => {
                this.reload(cx);
                this.start(None, cx);
                this.tab = Tab::Games;
            }
        });
        (form, subscription)
    }

    /// Looks through each prefix the profile's external games own for games
    /// a launcher there installed, off the UI thread: it reads every
    /// prefix's registry.
    #[cfg(proton)]
    fn scan(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.clone() else {
            self.found.clear();
            return;
        };
        let added: HashSet<PathBuf> = self.games.iter().filter_map(crate::core::executable_for).collect();
        let hosts: Vec<(String, PathBuf)> = self
            .games
            .iter()
            .filter(|game| game.prefix_of.is_none())
            .filter_map(|game| {
                let prefix = crate::core::prefix_for(&profile_id, &game.game_id)?;
                Some((game.game_id.clone(), prefix.path().to_path_buf()))
            })
            .collect();
        self._scan = Some(cx.spawn(async move |this, cx| {
            let found = tokio::task::spawn_blocking(move || {
                hosts
                    .into_iter()
                    .map(|(host, root)| {
                        let new: Vec<Detected> = crate::external::launchers::detect(&root)
                            .into_iter()
                            .filter(|game| !added.contains(&game.executable))
                            .collect();
                        (host, new)
                    })
                    .filter(|(_, new)| !new.is_empty())
                    .collect::<HashMap<_, _>>()
            })
            .await
            .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.found = found;
                cx.notify();
            });
        }));
    }

    /// Re-reads the list here and the library's copy of it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.games = self
            .profile_id
            .as_deref()
            .map(crate::external::list)
            .unwrap_or_default();
        #[cfg(proton)]
        self.scan(cx);
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.refresh_external(cx));
        cx.notify();
    }

    fn editing(&self, cx: &App) -> bool {
        self.form.as_ref().is_some_and(|(form, _)| {
            let form = form.read(cx);
            self.games.iter().any(|game| form.is_editing(&game.game_id))
        })
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.as_deref() else {
            return;
        };
        if let Err(err) = crate::external::remove(profile_id, id) {
            tracing::error!(profile_id, id, %err, "could not remove external game");
        }
        if self.form.as_ref().is_some_and(|(form, _)| form.read(cx).is_editing(id)) {
            self.start(None, cx);
            self.tab = Tab::Games;
        }
        self.reload(cx);
    }

    fn save_key(&mut self, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).text().trim().to_owned();
        self.key_status = t!("external.sources.saving").into_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(move || art::set_api_key(&key))
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.key_status = match result {
                    Ok(()) => t!("external.sources.saved").into_owned(),
                    Err(err) => err,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn game_list(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        if self.games.is_empty() {
            return theme::empty(t!("external.games.empty")).into_any_element();
        }
        let running = AppState::get(cx).background.read(cx).running.clone();
        let native = !Caps::get(cx).support(Feature::Proton).is_offered();
        let name_of = |id: &str| {
            self.games
                .iter()
                .find(|game| game.game_id == id)
                .map_or_else(String::new, |game| game.name.clone())
        };
        let (edit, remove) = (t!("external.games.edit"), t!("external.games.remove.button"));
        let mut rows: Vec<AnyElement> = Vec::with_capacity(self.games.len());
        for (index, game) in self.games.iter().enumerate() {
            let is_running = running.contains(&game.game_id);
            let hosted = self
                .games
                .iter()
                .filter(|other| other.prefix_of.as_deref() == Some(game.game_id.as_str()))
                .count();
            let remove_hint = match (&game.prefix_of, hosted) {
                (Some(host), _) => t!("external.games.remove.hosted", host = name_of(host)),
                (None, 0) if native => t!("external.games.remove.native"),
                (None, 0) => t!("external.games.remove.alone"),
                (None, 1) => t!("external.games.remove.launcher.one"),
                (None, count) => t!("external.games.remove.launcher.other", count = count),
            };
            let detail = match &game.prefix_of {
                Some(host) => format!(
                    "{} · {}",
                    t!("external.games.in_host", host = name_of(host)),
                    game.launch_executable.clone().unwrap_or_default()
                ),
                None => game.launch_executable.clone().unwrap_or_default(),
            };
            let edit_id = game.game_id.clone();
            let id = game.game_id.clone();
            rows.push(
                theme::list_row(("external-row", index), false)
                    .child(icon(game.art.icon.as_deref()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(game.name.clone()))
                            .child(theme::muted(detail).text_size(px(12.))),
                    )
                    .child(
                        button(("external-edit", index), ButtonKind::Secondary, edit.clone(), false)
                            .on_click(cx.listener(move |this, _, _, cx| this.start(Some(edit_id.clone()), cx))),
                    )
                    .child(
                        button(("external-remove", index), ButtonKind::Icon, remove.clone(), is_running)
                            .tooltip(tooltip(if is_running {
                                t!("external.games.remove.running")
                            } else {
                                remove_hint
                            }))
                            .when(!is_running, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| this.remove(&id, cx)))
                            }),
                    )
                    .into_any_element(),
            );
            #[cfg(proton)]
            for (found_index, found) in self.found.get(&game.game_id).into_iter().flatten().enumerate() {
                let (host, found_game) = (game.game_id.clone(), found.clone());
                rows.push(
                    theme::list_row(("external-found", index * 100 + found_index), false)
                        .pl(px(68.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().child(t!("external.games.found", host = game.name, game = found.name)))
                                .child(theme::muted(found.executable.display().to_string()).text_size(px(12.))),
                        )
                        .child(
                            button(
                                ("external-add-found", index * 100 + found_index),
                                ButtonKind::Primary,
                                t!("external.games.add_found"),
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_hosted(&host, found_game.clone(), cx);
                            })),
                        )
                        .into_any_element(),
                );
            }
        }
        theme::card().children(rows).into_any_element()
    }

    fn sources(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .child(theme::muted(t!("external.sources.hint")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mt_3()
                    .max_w(px(640.))
                    .child(div().flex_1().min_w_0().child(self.api_key.clone()))
                    .child(
                        button("sgdb-save", ButtonKind::Secondary, t!("external.sources.save"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.save_key(cx))),
                    )
                    .child(
                        button("sgdb-get-key", ButtonKind::Icon, t!("external.sources.get_key"), false).on_click(
                            |_, _, _| {
                                if let Err(err) = crate::desktop::open(art::SGDB_KEY_PAGE) {
                                    tracing::warn!(%err, "could not open the steamgriddb key page");
                                }
                            },
                        ),
                    ),
            )
            .when(!self.key_status.is_empty(), |column| {
                column.child(theme::muted(self.key_status.clone()).mt_2())
            })
    }
}

impl Render for ExternalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing(cx);
        let form_label = if editing {
            t!("external.tabs.edit")
        } else {
            t!("external.tabs.add")
        };
        let items = [
            (Tab::Form, form_label),
            (Tab::Games, t!("external.tabs.games")),
            (Tab::Sources, t!("external.tabs.sources")),
        ];
        let entity = cx.entity();
        let tabs = crate::widgets::tabs("external-tabs", &items, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| {
                // A launcher may have installed something since the last look.
                #[cfg(proton)]
                if tab == Tab::Games && this.tab != Tab::Games {
                    this.scan(cx);
                }
                this.tab = tab;
                cx.notify();
            });
        })
        .mt_6()
        .mb_6();

        let body = match (self.tab, &self.form) {
            (_, None) => theme::empty(t!("common.pick_profile")).into_any_element(),
            (Tab::Form, Some((form, _))) => div()
                .flex()
                .flex_col()
                .when(editing, |column| {
                    column.child(
                        div().flex().mb_3().child(
                            button("external-new", ButtonKind::Secondary, t!("external.add_another"), false)
                                .on_click(cx.listener(|this, _, _, cx| this.start(None, cx))),
                        ),
                    )
                })
                .child(form.clone())
                .into_any_element(),
            (Tab::Games, Some(_)) => self.game_list(cx).into_any_element(),
            (Tab::Sources, Some(_)) => self.sources(cx).into_any_element(),
        };

        theme::page(
            "external",
            div()
                .flex()
                .flex_col()
                .child(theme::h1(t!("external.title")))
                .child(theme::muted(if Caps::get(cx).support(Feature::Proton).is_offered() {
                    t!("external.intro")
                } else {
                    t!("external.intro_native")
                }))
                .child(tabs)
                .child(body),
        )
    }
}

fn icon(url: Option<&str>) -> impl IntoElement {
    let frame = div()
        .flex_none()
        .size(px(32.))
        .rounded(px(theme::RADIUS_SM))
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL));
    match url {
        Some(url) => frame.child(
            img(crate::art::source(url, gpui::size(32., 32.)))
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => frame,
    }
}
