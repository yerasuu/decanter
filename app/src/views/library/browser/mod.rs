//! The cover-art view: collections as horizontal shelves of capsules,
//! the way Steam's library home shows them.
//!
//! Mostly unfiltered — this is the "browse my shelves" view, and a
//! collection that looked half-empty because a filter was still set in
//! the sidebar would be misleading. Narrowing is what the sidebar list is
//! for; this is for looking at what you have.
//!
//! "Solo instalados" is the one exception, shared with the sidebar through
//! [`LibraryModel`]'s `filters` rather than owned by either: unlike
//! genre/achievement/search narrowing, which really is sidebar-specific,
//! "do I have this or not" is a property of the whole library the user
//! expects to hold everywhere, not just in one list.

mod derived;
mod page;
mod shelf;
mod slots;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, KeyDownEvent, ScrollHandle, SharedString, Task, Window, div, point, prelude::*, px,
    rgb,
};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use super::context_menu::MenuTarget;
use super::data::{self, Collections, LibraryStats};
use super::filters::{PlayState, Sort, pending_install_ids};
use super::{LibraryModel, LibraryPopup};
use crate::state::{AppState, DownloadKey};
use crate::theme;
use crate::widgets::nav;
use derived::derive;
use page::Page;
use shelf::{CARD_STRIDE, ROW_PADDING, visible_cards};
use slots::{load_slots, save_slots};

/// One row of the home layout. A shelf is a slot the user points at a
/// source, not a fixed view of one collection — which is what lets the
/// same collection appear twice, ordered two different ways.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Slot {
    /// A render handle only — handed out fresh on every load, never saved.
    #[serde(skip)]
    key: u32,
    /// Shelf id from [`shelves::build`](super::shelves::build), or `"all"` for the whole library.
    source: String,
    sort: Sort,
    /// Only the header is drawn, the way a sidebar shelf folds its list away.
    #[serde(default)]
    collapsed: bool,
}

/// What the sqlite-backed reads depend on. `render` runs on every artwork
/// arrival, so these are only re-read when one of them moves.
#[derive(Clone, PartialEq)]
struct OrganisationKey {
    profile: String,
    organisation: u32,
    /// Otherwise a session's playtime only shows up here after some
    /// unrelated change — the detail pane already reacts to this same
    /// counter for its own stat.
    playtime: u32,
}

struct Organisation {
    collections: Collections,
    favorites: HashSet<String>,
    hidden: HashSet<String>,
    /// Read once for the whole library: the capsules' meta lines come from
    /// it, and re-reading sqlite once per capsule would be hundreds of
    /// queries.
    stats: LibraryStats,
}

impl Organisation {
    fn load(profile_id: &str) -> Self {
        Self {
            collections: data::load_collections(profile_id),
            favorites: data::favorite_ids_in_profile(profile_id),
            hidden: data::hidden_ids_in_profile(profile_id),
            stats: data::library_stats(profile_id),
        }
    }
}

/// Everything the shelves are derived from, so they're rebuilt only when
/// one of these changed rather than on every frame.
#[derive(Clone, PartialEq)]
struct ShelvesKey {
    organisation: OrganisationKey,
    /// The library's own revision: a fetch batch reshapes the shelves but
    /// leaves the sqlite reads above as they were.
    games: u64,
    installed_only: bool,
    pending_installs: HashSet<DownloadKey>,
    slots: u64,
    /// The source names and capsule lines are translated.
    locale: String,
}

/// One capsule, already reduced to what gets drawn.
struct Capsule {
    id: SharedString,
    title: SharedString,
    cover: Option<SharedString>,
    meta: SharedString,
    installed: bool,
    /// Marked on the art so a shelf can be scanned for starred games
    /// without opening any of them.
    favorite: bool,
    is_mod: bool,
}

struct ShelfData {
    key: u32,
    collapsed: bool,
    source: SharedString,
    sort: Sort,
    capsules: Vec<Capsule>,
    installed: usize,
    dynamic: bool,
}

struct Derived {
    key: ShelvesKey,
    library_empty: bool,
    sources: Vec<(SharedString, SharedString)>,
    shelves: Vec<ShelfData>,
}

/// One shelf row's horizontal scroll, kept across renders by slot key.
#[derive(Default)]
struct RowState {
    scroll: ScrollHandle,
    hovered: bool,
    /// The running page animation; replacing it cancels the previous one so
    /// two quick clicks don't fight over the offset.
    paging: Option<Task<()>>,
    /// The capsules laid out this frame, as a range of the shelf's.
    shown: (usize, usize),
    /// Owned here rather than by each capsule, so the arrows can move the
    /// focus to a capsule in another row. Only the laid-out ones keep one.
    cards: HashMap<SharedString, FocusHandle>,
    /// The shelf header's entry to its page, which is where the focus comes back to.
    open_focus: Option<FocusHandle>,
}

impl RowState {
    fn focused(&self, capsules: &[Capsule], window: &Window) -> Option<usize> {
        let (first, last) = self.shown;
        capsules
            .get(first..last)?
            .iter()
            .position(|capsule| {
                self.cards
                    .get(&capsule.id)
                    .is_some_and(|handle| handle.is_focused(window))
            })
            .map(|index| first + index)
    }

    /// The first capsule wholly in view, which is where the arrows land
    /// coming from another row.
    fn first_in_view(&self) -> usize {
        (-self.scroll.offset().x.as_f32() / CARD_STRIDE).ceil().max(0.) as usize
    }

    /// Scrolls just far enough to show capsule `index` whole.
    fn reveal(&self, index: usize) {
        let offset = self.scroll.offset();
        let width = self.scroll.bounds().size.width.as_f32();
        let left = index as f32 * CARD_STRIDE;
        let right = ROW_PADDING.mul_add(2., left + CARD_STRIDE);
        let shown = -offset.x.as_f32();
        let target = if left < shown {
            left
        } else if right > shown + width {
            right - width
        } else {
            return;
        };
        let target = target.min(self.scroll.max_offset().x.as_f32()).max(0.);
        self.scroll.set_offset(point(px(-target), offset.y));
    }
}

pub struct BrowserView {
    model: Entity<LibraryModel>,
    /// The profile `slots` were loaded for. Restored once per profile, not
    /// per render — re-reading the saved layout every render would undo a
    /// drag the moment it landed — and keyed on the profile because the
    /// topbar's switcher changes it without recreating this view.
    profile: Option<String>,
    slots: Vec<Slot>,
    next_key: u32,
    /// Bumped on every slot change, so the derived shelves know to rebuild.
    slots_epoch: u64,
    /// Bumped whenever the library entity notifies.
    games_epoch: u64,
    organisation: Option<(OrganisationKey, Organisation)>,
    derived: Option<Derived>,
    /// The key a background rebuild is working on; the previous shelves
    /// stay on screen until it lands.
    deriving: Option<ShelvesKey>,
    rows: HashMap<u32, RowState>,
    /// Which shelf is being dragged, for the header-drag reordering Steam
    /// uses. Only trusted while GPUI still has a drag active, since a drag
    /// dropped outside any header never reports back.
    dragging: Option<u32>,
    scroll: ScrollHandle,
    /// The capsule last scrolled into view for having the focus, so a
    /// focused capsule is brought into view once rather than pulled back
    /// on every scroll of the wheel.
    revealed: Option<FocusHandle>,
    /// The shelf opened as a page, in place of the shelves.
    page: Option<Page>,
    /// The shelf whose entry takes the focus back once the page is closed.
    return_to: Option<u32>,
}

impl BrowserView {
    pub fn new(model: Entity<LibraryModel>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.library, |this, _, cx| {
            this.games_epoch += 1;
            cx.notify();
        })
        .detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        let mut library_shelves = state.session.read(cx).library_shelves;
        cx.observe(&state.session, move |this, session, cx| {
            let shelves = session.read(cx).library_shelves;
            if shelves != library_shelves {
                library_shelves = shelves;
                this.page = None;
                cx.notify();
            }
            let profile = session.read(cx).active_profile.clone();
            if profile != this.profile {
                this.load_profile(profile);
                cx.notify();
            }
        })
        .detach();
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let mut view = Self {
            model,
            profile: None,
            slots: Vec::new(),
            next_key: 0,
            slots_epoch: 0,
            games_epoch: 0,
            organisation: None,
            derived: None,
            deriving: None,
            rows: HashMap::new(),
            dragging: None,
            scroll: ScrollHandle::new(),
            revealed: None,
            page: None,
            return_to: None,
        };
        view.load_profile(state.session.read(cx).active_profile.clone());
        view
    }

    fn load_profile(&mut self, profile: Option<String>) {
        self.slots = profile.as_deref().map(load_slots).unwrap_or_default();
        self.next_key = self.slots.len() as u32;
        self.profile = profile;
        self.rows.clear();
        self.dragging = None;
        self.page = None;
        self.slots_epoch += 1;
    }

    /// Written back on every change. Selecting a game swaps this view out
    /// for the detail pane, and a restart drops it entirely — without this
    /// the arrangement survives neither.
    fn slots_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = &self.profile {
            save_slots(profile, &self.slots);
        }
        let slots = &self.slots;
        self.rows.retain(|key, _| slots.iter().any(|slot| slot.key == *key));
        self.slots_epoch += 1;
        cx.notify();
    }

    fn update_slot(&mut self, key: u32, change: impl FnOnce(&mut Slot), cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.key == key) {
            change(slot);
            self.slots_changed(cx);
        }
    }

    fn add_slot(&mut self, cx: &mut Context<Self>) {
        let key = self.next_key;
        self.next_key += 1;
        self.slots.push(Slot {
            key,
            source: "all".to_string(),
            sort: Sort::Title,
            collapsed: false,
        });
        self.slots_changed(cx);
    }

    fn remove_slot(&mut self, key: u32, cx: &mut Context<Self>) {
        self.slots.retain(|slot| slot.key != key);
        self.slots_changed(cx);
    }

    fn reorder(&mut self, from_key: u32, to_key: u32, cx: &mut Context<Self>) {
        self.dragging = None;
        let from = self.slots.iter().position(|slot| slot.key == from_key);
        let to = self.slots.iter().position(|slot| slot.key == to_key);
        if let (Some(from), Some(to)) = (from, to)
            && from != to
        {
            let moved = self.slots.remove(from);
            self.slots.insert(to, moved);
            self.slots_changed(cx);
        } else {
            cx.notify();
        }
    }

    /// Scrolls one shelf by most of its visible width, so a click always
    /// lands on a fresh set of capsules rather than nudging by some
    /// arbitrary amount. Clamped, so the last page is short rather than
    /// overshooting; eased over a few frames the way the webview's smooth
    /// scroll was.
    fn page_row(&mut self, key: u32, direction: f32, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get_mut(&key) else {
            return;
        };
        let handle = row.scroll.clone();
        let start = handle.offset();
        let width = handle.bounds().size.width;
        // GPUI's offset grows negative as the row scrolls right.
        let target = (start.x - width * (0.85 * direction))
            .min(px(0.))
            .max(-handle.max_offset().x);
        row.paging = Some(cx.spawn(async move |this, cx| {
            const FRAMES: u32 = 15;
            for frame in 1..=FRAMES {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let t = frame as f32 / FRAMES as f32;
                let eased = 1. - (1. - t).powi(3);
                handle.set_offset(point(start.x + (target - start.x) * eased, start.y));
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    /// Works out which capsules each row lays out this frame and gives each
    /// a focus handle, after scrolling a newly focused one into view.
    fn layout_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(derived) = &self.derived else {
            return;
        };
        if let Some(opened_from) = self.model.update(cx, |model, _| model.opened_from.take()) {
            window.focus(&opened_from, cx);
        }
        if self
            .page
            .as_ref()
            .is_some_and(|page| !derived.shelves.iter().any(|shelf| shelf.key == page.slot))
        {
            self.page = None;
        }
        if self.page.is_some() {
            self.layout_page(window, cx);
            return;
        }
        let mut revealed = None;
        for (position, shelf) in derived.shelves.iter().enumerate() {
            let Some(row) = self.rows.get_mut(&shelf.key) else {
                continue;
            };
            if let Some(index) = row.focused(&shelf.capsules, window) {
                let handle = row.cards.get(&shelf.capsules[index].id).cloned();
                if handle != self.revealed {
                    row.reveal(index);
                    self.scroll
                        .scroll_to_item(position + usize::from(derived.library_empty));
                }
                revealed = handle;
            }
            let open_focus = row.open_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true));
            if self.return_to == Some(shelf.key) {
                window.focus(open_focus, cx);
                self.scroll
                    .scroll_to_item(position + usize::from(derived.library_empty));
            }
            let viewport = row.scroll.bounds().size.width.as_f32();
            row.shown = visible_cards(shelf.capsules.len(), viewport, -row.scroll.offset().x.as_f32());
            let shown = &shelf.capsules[row.shown.0..row.shown.1];
            row.cards
                .retain(|id, handle| handle.is_focused(window) || shown.iter().any(|capsule| capsule.id == *id));
            for capsule in shown {
                row.cards
                    .entry(capsule.id.clone())
                    .or_insert_with(|| cx.focus_handle().tab_stop(true));
            }
        }
        self.return_to = None;
        self.revealed = revealed;
    }

    /// The arrows between capsules: sideways along a row, and up or down to
    /// the capsule in the same column of the next row that has any. Past the
    /// ends they're left to the window's own tab order.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(derived) = &self.derived else {
            return;
        };
        let shelves = &derived.shelves;
        let Some((position, index)) = shelves.iter().enumerate().find_map(|(position, shelf)| {
            let row = self.rows.get(&shelf.key)?;
            row.focused(&shelf.capsules, window).map(|index| (position, index))
        }) else {
            return;
        };
        let shelf = &shelves[position];
        let Some(row) = self.rows.get(&shelf.key) else {
            return;
        };
        let keystroke = &event.keystroke;
        if nav::is_menu_key(keystroke) {
            let bounds = row.scroll.bounds();
            let left = (index as f32).mul_add(CARD_STRIDE, ROW_PADDING) + row.scroll.offset().x.as_f32();
            let target = MenuTarget::single(
                &shelf.capsules[index].id,
                &shelf.capsules[index].title,
                point(bounds.left() + px(left + 24.), bounds.top() + px(48.)),
            );
            self.model
                .update(cx, |model, cx| model.open_popup(LibraryPopup::GameMenu(target), cx));
            cx.stop_propagation();
            return;
        }
        if keystroke.modifiers.modified() {
            return;
        }
        let target = match keystroke.key.as_str() {
            "left" => index.checked_sub(1).map(|index| (position, index)),
            "right" => (index + 1 < shelf.capsules.len()).then_some((position, index + 1)),
            "up" | "down" => {
                let column = index.saturating_sub(row.first_in_view());
                let filled = |shelf: &ShelfData| !shelf.capsules.is_empty() && !shelf.collapsed;
                let other = if keystroke.key == "up" {
                    shelves[..position].iter().rposition(filled)
                } else {
                    shelves[position + 1..]
                        .iter()
                        .position(filled)
                        .map(|other| other + position + 1)
                };
                other.map(|other| {
                    let shelf = &shelves[other];
                    let first = self.rows.get(&shelf.key).map_or(0, RowState::first_in_view);
                    (other, (first + column).min(shelf.capsules.len() - 1))
                })
            }
            _ => return,
        };
        let Some((position, index)) = target else {
            if matches!(keystroke.key.as_str(), "left" | "right") {
                cx.stop_propagation();
            }
            return;
        };
        let shelf = &shelves[position];
        let Some(row) = self.rows.get_mut(&shelf.key) else {
            return;
        };
        let handle = row
            .cards
            .entry(shelf.capsules[index].id.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        window.focus(&handle, cx);
        cx.stop_propagation();
    }

    /// Re-reads sqlite and rebuilds the shelves only when something they
    /// depend on moved; a no-op on most renders.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile.clone() else {
            self.derived = None;
            return;
        };
        let state = AppState::get(cx).clone();
        let background = state.background.read(cx);
        let organisation_key = OrganisationKey {
            profile,
            organisation: background.organisation,
            playtime: background.playtime_generation,
        };
        let key = ShelvesKey {
            organisation: organisation_key,
            games: self.games_epoch,
            installed_only: self.model.read(cx).filters.states.contains(&PlayState::Installed),
            // A queued or downloading game should already read as installed
            // here, not just once the manifest scan catches up.
            pending_installs: pending_install_ids(&background.installing),
            slots: self.slots_epoch,
            locale: rust_i18n::locale().to_string(),
        };
        if self.derived.as_ref().is_some_and(|derived| derived.key == key) || self.deriving.as_ref() == Some(&key) {
            return;
        }
        self.deriving = Some(key.clone());
        // The sqlite reads and the sorting/grouping of hundreds of entries
        // run off the UI thread; only a still-valid sqlite read is reused.
        let organisation = self
            .organisation
            .take()
            .filter(|(organisation_key, _)| *organisation_key == key.organisation);
        let games = state.library.read(cx).games.clone();
        let slots = self.slots.clone();
        let work = cx.background_spawn(async move {
            let organisation = organisation.unwrap_or_else(|| {
                let loaded = Organisation::load(&key.organisation.profile);
                (key.organisation.clone(), loaded)
            });
            let derived = derive(key, &games, &organisation.1, &slots);
            (organisation, derived)
        });
        cx.spawn(async move |this, cx| {
            let (organisation, derived) = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.organisation.is_none() {
                    this.organisation = Some(organisation);
                }
                if this.deriving.as_ref() != Some(&derived.key) {
                    return;
                }
                this.deriving = None;
                for slot in &this.slots {
                    this.rows.entry(slot.key).or_default();
                }
                this.derived = Some(derived);
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        self.layout_rows(window, cx);
        let dragging = self.dragging.filter(|_| cx.has_active_drag());
        let open = self.page.as_ref().and_then(|page| {
            let shelf = self
                .derived
                .as_ref()?
                .shelves
                .iter()
                .find(|shelf| shelf.key == page.slot)?;
            Some((page, shelf))
        });
        if let Some((page, shelf)) = open {
            let sources = self.derived.as_ref().map_or(&[][..], |derived| &derived.sources);
            return self.render_page(page, shelf, sources, cx).into_any_element();
        }
        let browser = div()
            .id("shelf-browser")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_key_down(cx.listener(Self::on_key))
            .pt(px(22.))
            .pb(px(56.));
        let Some(derived) = &self.derived else {
            return browser.into_any_element();
        };

        browser
            .when(derived.library_empty, |browser| {
                browser.child(theme::empty(t!("library.browser.nothing_yet")))
            })
            .children(
                derived
                    .shelves
                    .iter()
                    .map(|shelf| self.render_shelf(shelf, &derived.sources, dragging, cx)),
            )
            .child(
                nav::tab_stop(div().id("add-shelf"))
                    .mt(px(4.))
                    .mx(px(26.))
                    .px(px(14.))
                    .py(px(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(theme::LINE))
                    .rounded(px(theme::RADIUS))
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|button| {
                        button
                            .border_color(rgb(theme::ACCENT_BLUE))
                            .text_color(rgb(theme::ACCENT_BLUE))
                            .bg(rgb(theme::BG_PANEL_2))
                    })
                    .active(|button| button.mt(px(5.)).mb(px(-1.)))
                    .on_click(cx.listener(|this, _, _, cx| this.add_slot(cx)))
                    .child(t!("library.browser.add_shelf")),
            )
            .into_any_element()
    }
}
