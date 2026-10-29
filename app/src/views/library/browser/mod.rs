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
mod shelf;
mod slots;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{Context, Entity, ScrollHandle, SharedString, Task, Window, div, point, prelude::*, px, rgb};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use super::LibraryModel;
use super::data::{self, Collections, LibraryStats};
use super::filters::{PlayState, Sort, pending_install_ids};
use crate::state::{AppState, DownloadKey};
use crate::theme;
use derived::derive;
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
        cx.observe(&state.session, |this, session, cx| {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let dragging = self.dragging.filter(|_| cx.has_active_drag());
        let browser = div()
            .id("shelf-browser")
            .size_full()
            .overflow_y_scroll()
            .pt(px(22.))
            .pb(px(56.));
        let Some(derived) = &self.derived else {
            return browser;
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
                div()
                    .id("add-shelf")
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
    }
}
