//! The properties panel: everything Steam tucks behind right-click →
//! Propiedades for an installed game, in the same tabbed shape — General,
//! Actualizaciones, Archivos locales, Betas, plus two decanter adds on top:
//! Versiones (rollback to a specific historical build, not just Steam's
//! per-branch newest), Mods (third-party mods over the install) and
//! Compatibilidad (this game's own Proton pin; not on Windows, where games
//! run natively).
//!
//! A real second OS window rather than an in-app overlay — the same "its
//! own floating dialog, draggable and resizable independent of the
//! library" shape Steam's own Properties has. Nothing below reads the
//! main window's entities, only `crate::core`/`decanter_gog` free
//! functions and the game/account passed in, so the window stands alone.
//!
//! Each tab is its own entity, rebuilt whenever it's selected, so coming
//! back to a tab starts it fresh. Work a tab starts is detached rather
//! than owned by it: an install or repair half-done because the user
//! looked at another tab would leave the game worse off than either
//! outcome, so the work finishes and only its result is dropped.

mod cloud_saves;
mod compatibility;
mod dlc;
mod general;
mod local_files;
mod mods;
mod releases;
mod updates;

use std::rc::Rc;

use decanter_core::{Game, Storefront};
use gpui::{AnyView, App, AppContext as _, Context, Div, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::theme;
use crate::views::chrome;

/// Which game the window is about, shared by every tab.
struct Target {
    account_id: String,
    product_id: String,
    storefront: Storefront,
    title: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Updates,
    LocalFiles,
    CloudSaves,
    Dlc,
    Mods,
    Betas,
    Versions,
    Compatibility,
}

impl Tab {
    fn label(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::General => t!("library.properties.tabs.general"),
            Self::Updates => t!("library.properties.tabs.updates"),
            Self::LocalFiles => t!("library.properties.tabs.local_files"),
            Self::CloudSaves => t!("library.properties.tabs.cloud_saves"),
            Self::Dlc => t!("library.properties.tabs.dlc"),
            Self::Mods => t!("library.properties.tabs.mods"),
            Self::Betas => t!("library.properties.tabs.betas"),
            Self::Versions => t!("library.properties.tabs.versions"),
            Self::Compatibility => t!("library.properties.tabs.compatibility"),
        }
    }
}

const TABS: &[Tab] = &[
    Tab::General,
    Tab::Updates,
    Tab::LocalFiles,
    Tab::CloudSaves,
    Tab::Dlc,
    Tab::Mods,
    Tab::Betas,
    Tab::Versions,
    Tab::Compatibility,
];

/// What an external game has to show: nothing GOG-side (builds, branches,
/// DLC, cloud saves) applies to a file someone pointed at.
const EXTERNAL_TABS: &[Tab] = &[Tab::General, Tab::Compatibility];

/// Opens Propiedades for `game` in its own window. Fire-and-forget: unlike
/// an overlay, there's no state here for the caller to hold onto or close
/// later — the window manages its own lifetime, closed the same way any
/// other window is.
///
/// Nothing stops a second click from opening a second window on the same
/// game; Steam's own dialog doesn't either.
pub fn open_properties_window(game: Game, account_id: String, cx: &mut App) {
    let title = t!("library.properties.window_title", title = game.title).into_owned();
    let target = Rc::new(Target {
        account_id,
        product_id: game.id,
        storefront: game.storefront,
        title: game.title,
    });
    let options = chrome::window_options(&title, 640., 560., 480., 360., cx);
    if let Err(err) = cx.open_window(options, |window, cx| {
        cx.new(|cx| PropertiesWindow::new(target, title, window, cx))
    }) {
        tracing::error!(%err, "cannot open the properties window");
    }
}

struct PropertiesWindow {
    target: Rc<Target>,
    /// The title the window has, so it's only set again when the language changes.
    title: String,
    tab: Tab,
    pane: AnyView,
}

impl PropertiesWindow {
    fn new(target: Rc<Target>, title: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pane = build_pane(Tab::General, &target, window, cx);
        Self {
            target,
            title,
            tab: Tab::General,
            pane,
        }
    }

    fn select(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        // Re-clicking the open tab keeps it as it is, the same as the
        // webview build where setting an unchanged signal re-mounted nothing.
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        self.pane = build_pane(tab, &self.target, window, cx);
        cx.notify();
    }

    fn tabs(&self) -> &'static [Tab] {
        let tabs = if self.target.storefront == Storefront::Unspecified {
            EXTERNAL_TABS
        } else {
            TABS
        };
        if crate::launch::in_prefixes() {
            tabs
        } else {
            // Kept last in both lists so that it's the one dropped here.
            tabs.strip_suffix(&[Tab::Compatibility]).unwrap_or(tabs)
        }
    }

    fn nav(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("properties-nav")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(180.))
            .h_full()
            .bg(rgb(theme::BG_PANEL_3))
            .border_r_1()
            .border_color(rgb(theme::LINE))
            .overflow_y_scroll()
            .py_2()
            .children(self.tabs().iter().enumerate().map(|(index, &tab)| {
                let active = tab == self.tab;
                div()
                    .id(("properties-tab", index))
                    .flex()
                    .flex_none()
                    .items_center()
                    .w_full()
                    .px(px(18.))
                    .py(px(9.))
                    .border_l_3()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .when(active, |item| {
                        item.bg(rgb(theme::BG_PANEL_2))
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .border_color(rgb(theme::ACCENT_BLUE))
                    })
                    .when(!active, |item| {
                        item.text_color(rgb(theme::TEXT_MUTED))
                            .border_color(gpui::transparent_black())
                            .hover(|item| item.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_PRIMARY)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.select(tab, window, cx)))
                    .child(tab.label())
            }))
    }
}

fn build_pane(tab: Tab, target: &Rc<Target>, window: &mut Window, cx: &mut App) -> AnyView {
    let target = Rc::clone(target);
    match tab {
        Tab::General => cx.new(|_| general::GeneralTab::new(target)).into(),
        Tab::Updates => cx.new(|_| updates::UpdatesTab::new(target)).into(),
        Tab::LocalFiles => cx.new(|_| local_files::LocalFilesTab::new(target)).into(),
        Tab::CloudSaves => cx.new(|cx| cloud_saves::CloudSavesTab::new(target, window, cx)).into(),
        Tab::Dlc => cx.new(|_| dlc::DlcTab::new(target)).into(),
        Tab::Mods => cx.new(|cx| mods::ModsTab::new(target, cx)).into(),
        Tab::Betas => cx.new(|cx| releases::BetasTab::new(target, cx)).into(),
        Tab::Versions => cx.new(|cx| releases::VersionsTab::new(target, cx)).into(),
        Tab::Compatibility => cx.new(|cx| compatibility::CompatibilityTab::new(target, cx)).into(),
    }
}

impl Render for PropertiesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = t!("library.properties.window_title", title = self.target.title);
        if title != self.title {
            window.set_window_title(&title);
            self.title = title.into_owned();
        }
        let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(|this_window, _| this_window.remove_window());
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
            .child(chrome::logo(&t!("library.properties.title").to_uppercase()))
            .child(chrome::drag_handle(div().flex_1().h_full()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_4()
                    .child(chrome::window_controls(on_close)),
            );

        let body = div()
            .id("properties-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .pt(px(20.))
            .px_6()
            .pb_6()
            .child(self.pane.clone());

        // Same root styling the main window's `Root` sets, since a second
        // window inherits none of it.
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_DEEP))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .font_family(theme::SANS)
            .text_size(px(14.))
            .line_height(gpui::relative(1.5))
            .child(topbar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.nav(cx))
                    .child(body),
            )
            .children(chrome::resize_edges())
    }
}

/// `.properties-rows`: the label/value table at the top of most tabs.
fn rows() -> Div {
    div().flex().flex_col().gap_2().mb_4()
}

/// `.properties-row`: a muted label left, its value right.
fn row(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .text_size(px(13.))
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label.into()))
        .child(div().child(value.into()))
}

/// `.field-row`: a line of controls, at most 520px wide.
fn field_row() -> Div {
    div().flex().gap(px(10.)).mb_3().max_w(px(520.))
}

/// The tab body itself: a column whose blocks (rows, cards, error boxes)
/// stretch full width, the way block elements do.
fn pane() -> Div {
    div().flex().flex_col()
}

/// A lone button outside a `.field-row` keeps its own width, the way an
/// inline-block `button` does, instead of stretching across the column.
fn inline(element: impl IntoElement) -> Div {
    div().flex().child(element)
}

fn not_installed() -> Div {
    pane().child(theme::muted(t!("library.properties.not_installed")))
}

fn error_line(error: Option<&Text>) -> Option<Div> {
    error.map(theme::error)
}
