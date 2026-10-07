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

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use decanter_gog::LibraryEntry;
use gpui::{
    Animation, AnimationExt as _, BoxShadow, Context, Entity, FontWeight, MouseButton, ObjectFit, ScrollHandle,
    SharedString, Task, Window, div, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative, rgb,
};
use serde::{Deserialize, Serialize};

use super::context_menu::MenuTarget;
use super::data::{self, Collections, LibraryStats};
use super::filters::{PlayState, Sort, belongs_in_the_library, is_installed, pending_install_ids, sort_entries};
use super::{LibraryModel, LibraryPopup, shelves};
use crate::state::AppState;
use crate::widgets::select::SelectStyle;
use crate::widgets::{Select, tooltip};
use crate::{art, theme};

/// One row of the home layout. A shelf is a slot the user points at a
/// source, not a fixed view of one collection — which is what lets the
/// same collection appear twice, ordered two different ways.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Slot {
    /// A render handle only — handed out fresh on every load, never saved.
    #[serde(skip)]
    key: u32,
    /// Shelf id from [`shelves::build`], or `"all"` for the whole library.
    source: String,
    sort: Sort,
}

/// Sources that exist no matter what the user has made, so a saved shelf
/// pointing at one is always valid.
const BUILT_IN_SOURCES: [&str; 4] = ["all", "favorites", "uncategorized", "installed"];

const SORT_OPTIONS: [(&str, &str); 8] = [
    ("title", "Nombre"),
    ("played", "Mas jugado"),
    ("recent", "Jugado recientemente"),
    ("completion", "Logros"),
    ("release", "Lanzamiento"),
    ("size", "Tamano en disco"),
    ("rating", "Calificacion"),
    ("added", "Fecha agregada"),
];

/// `rgba(10, 22, 32, .88)`, the dark chip behind a capsule's pips.
const PIP_BG: u32 = 0x0a1620;

const CARD_GROUP: &str = "cover-card";
const HEAD_GROUP: &str = "browser-shelf-head";

fn load_slots(profile_id: &str) -> Vec<Slot> {
    let collection_ids: Vec<String> = data::load_collections(profile_id)
        .list
        .into_iter()
        .map(|collection| collection.id)
        .collect();
    restore_slots(
        data::view_state(profile_id, data::BROWSER_SHELVES).as_deref(),
        &collection_ids,
    )
}

/// The saved layout, with any shelf whose collection has since been
/// deleted dropped: a stale source renders as a permanently empty shelf
/// whose own name is missing from the picker, with no way to tell what it
/// was meant to be.
///
/// Unreadable JSON (hand-edited, or written by a much newer build) is
/// treated as nothing saved, the same as a first run.
fn restore_slots(raw: Option<&str>, collection_ids: &[String]) -> Vec<Slot> {
    let mut slots: Vec<Slot> = raw
        .and_then(|raw| {
            serde_json::from_str::<Vec<Slot>>(raw)
                .inspect_err(|err| tracing::warn!(error = %err, "failed to parse saved shelf layout, starting fresh"))
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|slot| {
            BUILT_IN_SOURCES.contains(&slot.source.as_str()) || collection_ids.iter().any(|id| id == &slot.source)
        })
        .collect();
    // Opens on the whole library: it's the one source that always exists,
    // where "favorites" is absent until something is starred and would
    // leave the first shelf pointing at nothing.
    if slots.is_empty() {
        slots.push(Slot {
            key: 0,
            source: "all".to_string(),
            sort: Sort::RecentlyPlayed,
        });
    }
    // Keys are handed out here, not restored: they only have to be unique
    // within this load, and `next_key` continues from the count.
    for (index, slot) in slots.iter_mut().enumerate() {
        slot.key = index as u32;
    }
    slots
}

fn save_slots(profile_id: &str, slots: &[Slot]) {
    if let Some(encoded) = save_slots_encode(slots) {
        data::save_view_state(profile_id, data::BROWSER_SHELVES, &encoded);
    }
}

fn save_slots_encode(slots: &[Slot]) -> Option<String> {
    serde_json::to_string(slots)
        .inspect_err(|err| tracing::warn!(error = %err, "failed to encode shelf layout, not saving"))
        .ok()
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
    pending_installs: HashSet<String>,
    slots: u64,
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

fn derive(key: ShelvesKey, games: &[LibraryEntry], organisation: &Organisation, slots: &[Slot]) -> Derived {
    let Organisation {
        collections,
        favorites,
        hidden,
        stats,
    } = organisation;
    let pending_installs = &key.pending_installs;
    // Only real games: a shelf of DLC capsules is a wall of near-identical
    // art for things that can't be launched.
    let entries: Vec<LibraryEntry> = games
        .iter()
        .filter(|entry| {
            entry.kind == decanter_gog::GameKind::Game && entry.parent_id.is_none() && belongs_in_the_library(entry)
        })
        .filter(|entry| !hidden.contains(&entry.game.id))
        .filter(|entry| !key.installed_only || is_installed(&entry.game, pending_installs))
        .cloned()
        .collect();
    let built = shelves::build(&entries, collections, favorites, stats, pending_installs);
    // Built-ins first, then the user's own collections — the order the
    // design prototype settled on, so the two don't drift.
    //
    // Listed unconditionally rather than picked out of `built`, which
    // drops a shelf the moment it's empty: a slot pointed at Favoritos
    // used to lose its own option the moment the last star came off, and
    // Instalados vanished as soon as a first collection retired the
    // install-state split. Either left the shelf blank with no way back.
    let mut sources: Vec<(SharedString, SharedString)> = vec![
        ("favorites".into(), "Favoritos".into()),
        ("all".into(), "Toda la biblioteca".into()),
        ("uncategorized".into(), "Sin coleccion".into()),
        ("installed".into(), "Instalados".into()),
    ];
    sources.extend(
        built
            .iter()
            .filter(|shelf| shelf.collection_id.is_some())
            .map(|shelf| (shelf.id.clone().into(), shelf.title.clone().into())),
    );

    let shelves = slots
        .iter()
        .map(|slot| {
            let pick = |keep: &dyn Fn(&LibraryEntry) -> bool| entries.iter().filter(|e| keep(e)).cloned().collect();
            let mut items: Vec<LibraryEntry> = match slot.source.as_str() {
                "all" => entries.clone(),
                "favorites" => pick(&|e| favorites.contains(&e.game.id)),
                "uncategorized" => pick(&|e| collections.is_uncategorized(&e.game.id)),
                "installed" => pick(&|e| is_installed(&e.game, pending_installs)),
                id => built
                    .iter()
                    .find(|shelf| shelf.id == id)
                    .map(|shelf| shelf.entries.clone())
                    .unwrap_or_default(),
            };
            sort_entries(&mut items, slot.sort, stats);
            let capsules: Vec<Capsule> = items
                .into_iter()
                .map(|entry| {
                    let game = entry.game;
                    Capsule {
                        meta: capsule_meta(stats.seconds(&game.id), stats.completion(&game.id)).into(),
                        installed: is_installed(&game, pending_installs),
                        favorite: favorites.contains(&game.id),
                        cover: game.cover_url.map(SharedString::from),
                        title: game.title.into(),
                        id: game.id.into(),
                    }
                })
                .collect();
            ShelfData {
                key: slot.key,
                source: slot.source.clone().into(),
                sort: slot.sort,
                installed: capsules.iter().filter(|capsule| capsule.installed).count(),
                capsules,
                dynamic: built.iter().any(|shelf| shelf.id == slot.source && shelf.dynamic),
            }
        })
        .collect();

    Derived {
        key,
        library_empty: entries.is_empty(),
        sources,
        shelves,
    }
}

/// A capsule's width plus the row's gap: every card takes the same room,
/// which is what lets a row lay out only the ones in view.
const CARD_GAP: f32 = 13.;
const CARD_STRIDE: f32 = 136. + CARD_GAP;
const ROW_PADDING: f32 = 26.;

/// One shelf row's horizontal scroll, kept across renders by slot key.
#[derive(Default)]
struct RowState {
    scroll: ScrollHandle,
    hovered: bool,
    /// The running page animation; replacing it cancels the previous one so
    /// two quick clicks don't fight over the offset.
    paging: Option<Task<()>>,
}

/// What a shelf header carries while it's dragged to a new place.
struct ShelfDrag {
    key: u32,
}

/// The header's ghost under the cursor, standing in for the webview's
/// drag image of the header bar.
struct ShelfGhost {
    label: SharedString,
}

impl Render for ShelfGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(26.)).opacity(0.6).child(
            theme::display(15.)
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(self.label.to_uppercase()),
        )
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

    fn render_shelf(
        &self,
        shelf: &ShelfData,
        sources: &[(SharedString, SharedString)],
        dragging: Option<u32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let key = shelf.key;
        let only_slot = self.slots.len() == 1;
        let total = shelf.capsules.len();
        let ghost_label = sources
            .iter()
            .find(|(id, _)| *id == shelf.source)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let view = cx.entity();

        let head = div()
            .id(("shelf-head", key))
            .group(HEAD_GROUP)
            .flex()
            .items_center()
            .gap(px(9.))
            .px(px(26.))
            .pb(px(11.))
            // Steam reorders shelves by dragging the header itself — the
            // whole bar is the handle.
            .cursor_grab()
            .on_drag(ShelfDrag { key }, move |drag, _, _, cx| {
                view.update(cx, |this, cx| {
                    this.dragging = Some(drag.key);
                    cx.notify();
                });
                let label = ghost_label.clone();
                cx.new(|_| ShelfGhost { label })
            })
            .drag_over::<ShelfDrag>(move |style, drag, _, _| {
                if drag.key == key {
                    return style;
                }
                style.shadow(vec![BoxShadow {
                    color: rgb(theme::ACCENT_BLUE).into(),
                    offset: point(px(0.), px(-2.)),
                    blur_radius: px(0.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .on_drop(cx.listener(move |this, drag: &ShelfDrag, _, cx| this.reorder(drag.key, key, cx)))
            .child(
                div()
                    .id(("shelf-grip", key))
                    .flex_none()
                    .text_size(px(13.))
                    .line_height(relative(1.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .group_hover(HEAD_GROUP, |grip| grip.text_color(rgb(theme::TEXT_MUTED)))
                    .tooltip(tooltip("Arrastrar para reordenar"))
                    .child("⠿"),
            )
            // The source picker *is* the shelf's title.
            .child(
                Select::new(("shelf-source", key), sources.iter().cloned(), shelf.source.clone())
                    .style(SelectStyle::Heading)
                    .on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                        this.update_slot(key, |slot| slot.source = value.to_string(), cx);
                    })),
            )
            .when(shelf.dynamic, |head| {
                head.child(
                    div()
                        .flex_none()
                        .text_size(px(8.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(theme::ACCENT_BLUE))
                        .border_1()
                        .border_color(rgb(theme::ACCENT_BLUE_DIM))
                        .rounded(px(8.))
                        .px(px(4.))
                        .child("AUTO"),
                )
            })
            .child(
                Select::new(
                    ("shelf-sort", key),
                    SORT_OPTIONS
                        .iter()
                        .map(|&(value, label)| (SharedString::from(value), SharedString::from(label))),
                    shelf.sort.value(),
                )
                .style(SelectStyle::Small)
                .on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                    this.update_slot(key, |slot| slot.sort = Sort::from_value(value), cx);
                })),
            )
            .child(
                theme::mono()
                    .flex_none()
                    .ml_auto()
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .child(format!("{}/{total} instalados", shelf.installed)),
            )
            .child(
                div()
                    .id(("shelf-remove", key))
                    .flex_none()
                    .px(px(8.))
                    .py(px(5.))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .rounded(px(theme::RADIUS_SM))
                    .text_size(px(12.))
                    .line_height(relative(1.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .tooltip(tooltip("Quitar estanteria"))
                    .when(only_slot, |button| button.opacity(0.3).cursor_not_allowed())
                    .when(!only_slot, |button| {
                        button
                            .cursor_pointer()
                            .hover(|button| {
                                button
                                    .text_color(rgb(theme::ERROR_TEXT))
                                    .bg(rgb(theme::BG_PANEL_2))
                                    .border_color(rgb(theme::ERROR_LINE))
                            })
                            .active(|button| button.mt(px(1.)).mb(px(-1.)))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_slot(key, cx)))
                    })
                    .child(crate::widgets::emoji(crate::widgets::Emoji::Wastebasket, 12.)),
            );

        let body = if shelf.capsules.is_empty() {
            div()
                .mx(px(26.))
                .p(px(16.))
                .border_1()
                .border_dashed()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS))
                .text_size(px(12.))
                .text_color(rgb(theme::TEXT_DIM))
                .child("Nada en esta estanteria.")
                .into_any_element()
        } else {
            self.render_row(shelf, cx).into_any_element()
        };

        div()
            .mb(px(30.))
            .when(dragging == Some(key), |shelf| shelf.opacity(0.45))
            .child(head)
            .child(body)
    }

    /// One shelf's scrolling row of capsules.
    ///
    /// One row that scrolls sideways rather than a wrapping grid: it keeps
    /// each shelf to a predictable height so several are visible at once.
    /// GPUI draws no scrollbar, which is what Steam does here too — the row
    /// is paged by the edge arrows (or a sideways wheel) instead.
    fn render_row(&self, shelf: &ShelfData, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let key = shelf.key;
        let (scroll, hovered) = match self.rows.get(&key) {
            Some(row) => (row.scroll.clone(), row.hovered),
            None => (ScrollHandle::new(), false),
        };
        // A pixel of slack either way: an eased scroll lands a hair off the
        // exact end often enough that comparing outright leaves the arrow
        // lit. A row not laid out yet reports a zero max offset, so it reads
        // as at both ends and neither arrow lights up on a shelf that fits.
        let offset = scroll.offset().x;
        let at_start = offset >= px(-1.);
        let at_end = -offset >= scroll.max_offset().x - px(1.);

        let arrow = |left: bool| {
            let (id, label, glyph, direction) = if left {
                ("row-arrow-left", "Ver anteriores", "‹", -1.)
            } else {
                ("row-arrow-right", "Ver siguientes", "›", 1.)
            };
            let fade = theme::alpha(theme::BG_DEEP, 0.);
            div()
                .id((id, key))
                .absolute()
                .top_0()
                .bottom(px(18.))
                .w(px(46.))
                .when(left, |arrow| arrow.left_0())
                .when(!left, |arrow| arrow.right_0())
                .flex()
                .items_center()
                .justify_center()
                .bg(linear_gradient(
                    if left { 90. } else { 270. },
                    linear_color_stop(rgb(theme::BG_DEEP), 0.34),
                    linear_color_stop(fade, 1.),
                ))
                .text_size(px(20.))
                .line_height(relative(1.))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .cursor_pointer()
                .hover(|arrow| arrow.text_color(rgb(theme::ACCENT_BLUE)))
                .active(|arrow| arrow.top(px(1.)).bottom(px(17.)))
                // A click on the arrow mustn't also open the capsule behind
                // it. Not `block_mouse_except_scroll`: that also takes hover
                // away from the viewport this arrow lives in, which removed
                // the arrow the moment the pointer reached it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .tooltip(tooltip(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.page_row(key, direction, cx);
                }))
                .child(glyph)
                .with_animation(
                    (id, key),
                    Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_in_out),
                    |arrow, delta| arrow.opacity(delta),
                )
        };

        // Only the capsules in view (and a few either side) are laid out:
        // a "Toda la biblioteca" shelf is hundreds of covers, and laying
        // all of them out on every hover or progress tick was the slowdown.
        // Spacers stand in for the rest so the scroll range stays whole.
        let count = shelf.capsules.len();
        let viewport = scroll.bounds().size.width.as_f32();
        // Not laid out yet on the first frame; a wide guess beats a blank row.
        let viewport = if viewport > 0. { viewport } else { 2560. };
        let scrolled = -offset.as_f32();
        // A full page laid out past either edge: a page arrow moves the row
        // by most of its width, and with any less the capsules it brought
        // in arrived still waiting on their art.
        let overscan = (viewport / CARD_STRIDE).ceil() as isize;
        let first = (((scrolled - ROW_PADDING) / CARD_STRIDE).floor() as isize - overscan).max(0) as usize;
        let last = ((((scrolled + viewport) / CARD_STRIDE).ceil() as isize + overscan).max(0) as usize).min(count);
        let first = first.min(last);
        let spacer = |cards: usize| div().flex_none().w(px(cards as f32 * CARD_STRIDE - CARD_GAP));

        let model = self.model.clone();
        div()
            .id(("browser-viewport", key))
            .relative()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if let Some(row) = this.rows.get_mut(&key) {
                    row.hovered = *hovered;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id(("browser-row", key))
                    .flex()
                    .gap(px(13.))
                    .pt(px(4.))
                    .px(px(26.))
                    .pb(px(10.))
                    .overflow_x_scroll()
                    // Otherwise a vertical wheel over a row pages it sideways
                    // instead of scrolling the browser past it.
                    .restrict_scroll_to_axis()
                    .track_scroll(&scroll)
                    // Windowing below depends on the offset, so a scroll
                    // has to re-render the row, not just repaint it.
                    .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                    .when(first > 0, |row| row.child(spacer(first)))
                    .children(
                        shelf.capsules[first..last]
                            .iter()
                            .map(|capsule| cover_card(capsule, &model)),
                    )
                    .when(last < count, |row| row.child(spacer(count - last))),
            )
            // An arrow with nothing left behind it is taken out entirely
            // rather than lighting up on hover and doing nothing.
            .when(hovered && !at_start, |viewport| viewport.child(arrow(true)))
            .when(hovered && !at_end, |viewport| viewport.child(arrow(false)))
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
                browser.child(theme::empty("Todavia no hay nada que mostrar."))
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
                    .child("+ Anadir estanteria"),
            )
    }
}

fn cover_card(capsule: &Capsule, model: &Entity<LibraryModel>) -> impl IntoElement {
    let click_model = model.clone();
    let menu_model = model.clone();
    let click_id = capsule.id.clone();
    let menu_id = capsule.id.clone();
    let menu_title = capsule.title.clone();
    let pip = |id: &'static str, glyph: &'static str, color: u32, label: &'static str| {
        div()
            .id(id)
            .bg(theme::alpha(PIP_BG, 0.88))
            .rounded(px(theme::RADIUS_SM))
            .px(px(5.))
            .py(px(1.))
            .text_size(px(10.))
            .line_height(relative(1.45))
            .text_color(rgb(color))
            .tooltip(tooltip(label))
            .child(glyph)
    };

    // Art is missing often enough that the title has to be legible without
    // it, so it's drawn over the art in both cases and the fallback only
    // changes what's behind it.
    let art = div()
        .id("cover-art")
        .relative()
        .w(px(136.))
        .h(px(204.))
        .rounded(px(theme::RADIUS))
        .flex()
        .items_end()
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL))
        .when(capsule.cover.is_none(), |art| {
            art.bg(rgb(theme::BG_PANEL_2)).shadow(vec![BoxShadow {
                color: rgb(theme::LINE).into(),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
                inset: true,
            }])
        })
        .group_hover(CARD_GROUP, |art| {
            art.top(px(-4.)).shadow(vec![
                BoxShadow {
                    color: rgb(theme::ACCENT_BLUE).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(2.),
                    inset: false,
                },
                BoxShadow {
                    color: gpui::black().opacity(0.45),
                    offset: point(px(0.), px(10.)),
                    blur_radius: px(22.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ])
        })
        .group_active(CARD_GROUP, |art| art.top(px(-1.)))
        .when_some(capsule.cover.as_deref(), |art, cover| {
            art.child(
                img(art::source(cover, gpui::size(136., 204.)))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(theme::RADIUS))
                    .object_fit(ObjectFit::Cover)
                    .with_fallback(|| div().into_any_element()),
            )
        })
        .child(div().absolute().top_0().left_0().size_full().bg(linear_gradient(
            180.,
            linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.42),
            linear_color_stop(theme::alpha(theme::BG_DEEP, 0.86), 1.),
        )))
        .child(
            // Three lines is what the scrim covers; a longer title would
            // climb out of the dark and back onto the artwork.
            theme::display(14.)
                .relative()
                .w_full()
                .p(px(11.))
                .line_height(relative(1.15))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .line_clamp(3)
                .child(capsule.title.clone()),
        );

    div()
        .id(capsule.id.clone())
        .group(CARD_GROUP)
        .relative()
        .flex_none()
        .w(px(136.))
        .cursor_pointer()
        .tooltip(tooltip(capsule.title.clone()))
        .on_click(move |_, _, cx| {
            let id = click_id.to_string();
            click_model.update(cx, |model, cx| model.select(Some(id), cx));
        })
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            cx.stop_propagation();
            let target = MenuTarget {
                product_ids: vec![menu_id.to_string()],
                title: menu_title.to_string(),
                position: event.position,
            };
            menu_model.update(cx, |model, cx| model.open_popup(LibraryPopup::GameMenu(target), cx));
        })
        .child(art)
        .child(
            div()
                .absolute()
                .top(px(7.))
                .right(px(7.))
                .flex()
                .gap(px(4.))
                .when(capsule.installed, |pips| {
                    pips.child(pip("cover-installed", "▶", theme::ACCENT_GREEN, "Instalado"))
                })
                .when(capsule.favorite, |pips| {
                    pips.child(pip("cover-favorite", "★", theme::ACCENT_GOLD, "Favorito"))
                }),
        )
        .child(
            div()
                .mt(px(7.))
                .text_size(px(11.))
                .text_color(rgb(theme::TEXT_MUTED))
                .truncate()
                .group_hover(CARD_GROUP, |meta| meta.text_color(rgb(theme::TEXT_PRIMARY)))
                .child(capsule.meta.clone()),
        )
}

/// The line under a capsule: time played and how much of the achievement
/// set is done. A game never launched says so rather than showing `0.0 h`,
/// and one with nothing cached simply omits the percentage — it isn't 0%
/// of anything.
fn capsule_meta(seconds: i64, completion: Option<f64>) -> String {
    let played = match seconds {
        0 => "Sin jugar".to_string(),
        _ if seconds < 3600 => format!("{} min", (seconds / 60).max(1)),
        _ => format!("{:.1} h", seconds as f64 / 3600.0),
    };
    match completion {
        Some(value) => format!("{played} · {:.0}%", value * 100.0),
        None => played,
    }
}
