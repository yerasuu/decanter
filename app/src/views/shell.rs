//! The main window's frame: topbar (logo, sections, profile switcher,
//! settings, window controls), the current section, and the download
//! strip along the bottom.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use decanter_core::Profile;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, SharedString, StyleRefinement, Subscription, Window, div, point,
    prelude::*, px, rgb,
};
use rust_i18n::t;

use super::chrome;
use super::downloads::DownloadsView;
use super::external::ExternalView;
use super::library::LibraryView;
use super::profile_form::{self, ProfileForm};
use super::ps::PsView;
use super::settings::SettingsView;
use super::storage::StorageView;
use crate::state::{AppState, InstallStatus};
use crate::theme;
use crate::widgets::{Select, menu, progress_bar, select::SelectStyle, tooltip};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Storage,
    Downloads,
    Ps,
    Settings,
    External,
}

/// Sentinel select value: not a real profile id (those are uuids), so it
/// can never collide with one.
const NEW_PROFILE_OPTION: &str = "__new__";
/// Sentinel select value for a library whose disk isn't there.
const UNAVAILABLE_OPTION: &str = "__unavailable__";

pub struct Shell {
    page: Page,
    library: Entity<LibraryView>,
    /// Built on first visit and kept, so coming back to a section finds it
    /// as it was left.
    storage: Option<Entity<StorageView>>,
    downloads: Option<Entity<DownloadsView>>,
    ps: Option<Entity<PsView>>,
    settings: Option<Entity<SettingsView>>,
    external: Option<Entity<ExternalView>>,
    profiles: Vec<(PathBuf, Option<Vec<Profile>>)>,
    new_profile: Option<(Entity<ProfileForm>, Subscription)>,
    on_close: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl Shell {
    pub fn new(on_close: Rc<dyn Fn(&mut Window, &mut App)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        let mut external_request = state.session.read(cx).external_request;
        cx.observe_in(&state.session, window, move |this, session, window, cx| {
            this.reload_profiles();
            let (request, editing) = {
                let session = session.read(cx);
                (session.external_request, session.external_edit.clone())
            };
            if request != external_request {
                external_request = request;
                this.navigate(Page::External, window, cx);
                if let Some(view) = &this.external {
                    view.update(cx, |view, cx| view.start(editing, cx));
                }
            }
            cx.notify();
        })
        .detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        let mut this = Self {
            page: Page::Library,
            library: cx.new(|cx| LibraryView::new(window, cx)),
            storage: None,
            downloads: None,
            ps: None,
            settings: None,
            external: None,
            profiles: Vec::new(),
            new_profile: None,
            on_close,
        };
        this.reload_profiles();
        this
    }

    fn reload_profiles(&mut self) {
        self.profiles = crate::core::profiles_by_library();
    }

    fn library_of(&self, profile_id: &str) -> Option<&Path> {
        self.profiles.iter().find_map(|(root, profiles)| {
            profiles
                .iter()
                .flatten()
                .any(|profile| profile.id == profile_id)
                .then_some(root.as_path())
        })
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page == Page::Library {
            // Even while already there: the click is how you get back from
            // a game's detail to the shelf browser.
            let session = AppState::get(cx).session.clone();
            session.update(cx, |session, cx| {
                session.library_home = session.library_home.wrapping_add(1);
                cx.notify();
            });
        }
        match page {
            Page::Library => {}
            Page::Storage => match &self.storage {
                Some(storage) => storage.update(cx, StorageView::rescan),
                None => self.storage = Some(cx.new(StorageView::new)),
            },
            Page::Downloads => {
                self.downloads.get_or_insert_with(|| cx.new(DownloadsView::new));
            }
            Page::Ps => {
                self.ps.get_or_insert_with(|| cx.new(PsView::new));
                let session = AppState::get(cx).session.clone();
                session.update(cx, |session, cx| {
                    session.ps_home = session.ps_home.wrapping_add(1);
                    cx.notify();
                });
            }
            Page::Settings => {
                if self.settings.is_none() {
                    self.settings = Some(cx.new(|cx| SettingsView::new(window, cx)));
                }
            }
            Page::External => {
                self.external.get_or_insert_with(|| cx.new(ExternalView::new));
            }
        }
        self.page = page;
        cx.notify();
    }

    fn switch_profile(&mut self, chosen: &str, window: &mut Window, cx: &mut Context<Self>) {
        if chosen == NEW_PROFILE_OPTION {
            let form = cx.new(|cx| ProfileForm::new(profile_form::Layout::Popup, window, cx));
            let subscription = cx.subscribe(&form, |this, _, _: &profile_form::Done, cx| {
                this.new_profile = None;
                cx.notify();
            });
            self.new_profile = Some((form, subscription));
            cx.notify();
            return;
        }
        if chosen == UNAVAILABLE_OPTION {
            return;
        }
        if let Some(root) = self
            .library_of(chosen)
            .filter(|root| decanter_core::bootstrap::active_root().as_deref() != Some(*root))
        {
            Self::switch_library(root, chosen, cx);
            return;
        }
        match crate::core::db().and_then(|db| db.set_active_profile(chosen).map_err(crate::core::StoreError::Query)) {
            Ok(()) => {
                tracing::info!(profile_id = chosen, "active profile switched");
                decanter_core::bootstrap::remember_profile(chosen);
                let session = AppState::get(cx).session.clone();
                let chosen = chosen.to_owned();
                session.update(cx, |session, cx| {
                    session.active_profile = Some(chosen);
                    cx.notify();
                });
            }
            Err(err) => tracing::warn!(profile_id = chosen, error = %err, "failed to switch active profile"),
        }
    }

    fn switch_library(root: &Path, chosen: &str, cx: &mut Context<Self>) {
        if Self::busy(cx) {
            tracing::info!(
                profile_id = chosen,
                "not switching library while a game runs or installs"
            );
            cx.notify();
            return;
        }
        match crate::core::switch_library(root, chosen) {
            Ok(()) => {
                tracing::info!(profile_id = chosen, root = %root.display(), "switched to a profile in another library");
                let session = AppState::get(cx).session.clone();
                session.update(cx, |session, cx| {
                    session.reload();
                    cx.notify();
                });
            }
            Err(err) => tracing::warn!(profile_id = chosen, error = %err, "failed to switch library"),
        }
    }

    fn nav_link(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        page: Page,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.page == page;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.))
            .px_3()
            .py(px(6.))
            .rounded(px(theme::RADIUS_SM))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .cursor_pointer()
            .when(active, |link| {
                link.text_color(rgb(theme::TEXT_BRIGHT)).bg(rgb(theme::BG_PANEL))
            })
            .when(!active, |link| {
                link.text_color(rgb(theme::TEXT_MUTED))
                    .hover(|link| link.text_color(rgb(theme::TEXT_PRIMARY)).bg(rgb(theme::BG_PANEL_2)))
            })
            .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
            .child(label.into())
    }

    /// A game started from the active library writes its playtime and
    /// achievements to the active database when it exits, and an install
    /// its records when it ends: switching library first sends them to
    /// another one.
    fn busy(cx: &App) -> bool {
        let background = AppState::get(cx).background.read(cx);
        !background.running.is_empty()
            || background
                .installing
                .values()
                .any(|status| !matches!(status, crate::state::InstallStatus::Failed(_)))
    }

    fn profile_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = AppState::get(cx).session.read(cx).active_profile.clone();
        let active_root = decanter_core::bootstrap::active_root();
        let busy = Self::busy(cx);
        // The active profile's initial, so which account is in play reads
        // at a glance instead of only from the select's current text.
        let initial = active
            .as_deref()
            .and_then(|id| {
                self.profiles
                    .iter()
                    .flat_map(|(_, profiles)| profiles.iter().flatten())
                    .find(|profile| profile.id == id)
            })
            .and_then(|profile| profile.name.chars().next())
            .map_or_else(|| "?".to_owned(), |first| first.to_uppercase().to_string());
        let options = self
            .profiles
            .iter()
            .flat_map(|(root, profiles)| {
                let elsewhere = active_root.as_ref() != Some(root);
                let unavailable = profiles.is_none().then(|| {
                    (
                        SharedString::from(UNAVAILABLE_OPTION),
                        SharedString::from(t!("chrome.profiles.unavailable", path = root.display())),
                    )
                });
                profiles
                    .iter()
                    .flatten()
                    .map(move |profile| {
                        let label = if elsewhere && busy {
                            t!("chrome.profiles.busy", name = profile.name, path = root.display()).into_owned()
                        } else if elsewhere {
                            t!("chrome.profiles.elsewhere", name = profile.name, path = root.display()).into_owned()
                        } else {
                            profile.name.clone()
                        };
                        (SharedString::from(profile.id.clone()), SharedString::from(label))
                    })
                    .chain(unavailable)
            })
            .chain([(NEW_PROFILE_OPTION.into(), t!("chrome.profiles.new").into())]);
        let entity = cx.entity();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(24.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme::RADIUS_SM))
                    .bg(gpui::linear_gradient(
                        135.,
                        gpui::linear_color_stop(rgb(theme::ACCENT_BLUE_DIM), 0.),
                        gpui::linear_color_stop(rgb(theme::BG_PANEL_LIGHT), 1.),
                    ))
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child(initial),
            )
            .child(
                Select::new("profile-switcher", options, active.unwrap_or_default())
                    .style(SelectStyle::Compact)
                    .on_change(move |chosen, window, cx| {
                        entity.update(cx, |this, cx| this.switch_profile(chosen, window, cx));
                    }),
            )
    }

    fn download_bar(cx: &App) -> Option<impl IntoElement + use<>> {
        let state = AppState::get(cx);
        let installing = &state.background.read(cx).installing;
        let games = &state.library.read(cx).games;
        // Only queued/downloading entries: a failure already has its own
        // resolution step (Descartar) in the detail pane and Descargas.
        let mut active: Vec<_> = installing
            .iter()
            .filter(|(_, status)| !matches!(status, InstallStatus::Failed(_)))
            .collect();
        if active.is_empty() {
            return None;
        }
        active.sort_by(|a, b| a.0.cmp(b.0));
        let items = active.into_iter().map(|(product_id, status)| {
            let title = games
                .iter()
                .find(|entry| entry.game.id == *product_id)
                .map_or_else(|| product_id.clone(), |entry| entry.game.title.clone());
            let label = div()
                .text_color(rgb(theme::TEXT_PRIMARY))
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_nowrap()
                .child(t!("chrome.downbar.downloading", title = title));
            let item = div().flex().flex_none().items_center().gap_2().child(label);
            match status {
                InstallStatus::Installing { bytes_per_second, .. } => {
                    let fraction = status.fraction();
                    item.child(div().w(px(110.)).child(progress_bar(
                        SharedString::from(format!("dl-{product_id}")),
                        Some(fraction.unwrap_or(1.)),
                        4.,
                    )))
                    .child(match fraction {
                        Some(fraction) => format!(
                            "{}% · {}",
                            (fraction * 100.0) as u32,
                            crate::format::rate(*bytes_per_second)
                        ),
                        None => crate::format::rate(*bytes_per_second),
                    })
                }
                InstallStatus::Queued | InstallStatus::Failed(_) => item.child(t!("common.queued")),
            }
        });
        Some(
            div()
                .id("downbar")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(18.))
                .px_4()
                .py(px(6.))
                .text_size(px(11.))
                .text_color(rgb(theme::TEXT_MUTED))
                .bg(rgb(theme::BG_PANEL_2))
                .border_t_1()
                .border_color(rgb(theme::LINE))
                .overflow_x_scroll()
                .children(items),
        )
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let installing = AppState::get(cx).background.read(cx).installing.len();
        let downloads_link = self
            .nav_link("nav-downloads", t!("chrome.nav.downloads"), Page::Downloads, cx)
            .when(installing > 0, |link| {
                link.child(
                    theme::mono()
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(8.))
                        .bg(rgb(theme::ACCENT_BLUE))
                        .text_color(rgb(theme::TEXT_ON_BLUE))
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(installing.to_string()),
                )
            });
        let topbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_6()
            .h(px(52.))
            .px(px(18.))
            .bg(rgb(theme::BG_PANEL_3))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(chrome::logo("DECANTER"))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_1()
                    .child(self.nav_link("nav-library", t!("chrome.nav.library"), Page::Library, cx))
                    .child(self.nav_link("nav-ps", "PS", Page::Ps, cx))
                    .child(self.nav_link("nav-storage", t!("chrome.nav.storage"), Page::Storage, cx))
                    .child(downloads_link),
            )
            // Empty on purpose: the window's only plain move surface, never
            // covered by a link, the profile switcher or the window buttons.
            .child(chrome::drag_handle(div().flex_1().h_full()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_4()
                    .child(self.profile_switcher(cx))
                    .child(
                        // A plain link in the webview build, so plain link
                        // colour: no padding, hover or active state of its own.
                        div()
                            .id("nav-settings")
                            .cursor_pointer()
                            .text_color(rgb(theme::ACCENT_BLUE))
                            .tooltip(tooltip(t!("settings.title")))
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(Page::Settings, window, cx)))
                            .child("⚙"),
                    )
                    .child(chrome::window_controls(Rc::clone(&self.on_close))),
            );

        // Cached: a page re-renders when it notifies, not every time a
        // hover or a tick elsewhere in the window redraws the shell.
        let page = || StyleRefinement::default().size_full();
        let body: AnyElement = match self.page {
            Page::Library => self.library.clone().cached(page()).into_any_element(),
            Page::Storage => self.storage.clone().map_or_else(
                || div().into_any_element(),
                |view| view.cached(page()).into_any_element(),
            ),
            Page::Downloads => self.downloads.clone().map_or_else(
                || div().into_any_element(),
                |view| view.cached(page()).into_any_element(),
            ),
            Page::Ps => self.ps.clone().map_or_else(
                || div().into_any_element(),
                |view| view.cached(page()).into_any_element(),
            ),
            Page::Settings => self.settings.clone().map_or_else(
                || div().into_any_element(),
                |view| view.cached(page()).into_any_element(),
            ),
            Page::External => self.external.clone().map_or_else(
                || div().into_any_element(),
                |view| view.cached(page()).into_any_element(),
            ),
        };

        // A floating popup, not inline in the topbar: the topbar is a fixed
        // 52px and an input, two buttons and an error line don't fit in it.
        let new_profile = self.new_profile.as_ref().map(|(form, _)| {
            let viewport = window.viewport_size();
            let entity = cx.entity();
            menu::popup(
                point(viewport.width - px(18. + 260.), px(60.)),
                form.clone(),
                window,
                Rc::new(move |_, cx| {
                    entity.update(cx, |this, cx| {
                        this.new_profile = None;
                        cx.notify();
                    });
                }),
            )
        });

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(topbar)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .children(Self::download_bar(cx))
            .children(new_profile)
            .children(chrome::resize_edges())
    }
}
