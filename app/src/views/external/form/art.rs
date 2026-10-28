use std::borrow::Cow;

use decanter_core::GameArt;
use gpui::{
    AnyElement, Context, Div, FontWeight, MouseButton, ObjectFit, SharedString, div, img, linear_color_stop,
    linear_gradient, point, prelude::*, px, relative, rgb,
};
use rust_i18n::t;

use super::{GameForm, Search};
use crate::external::art::{Candidate, Candidates, Source};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, tooltip};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArtKind {
    Cover,
    Hero,
    Icon,
}

impl ArtKind {
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Cover => t!("external.form.art.cover"),
            Self::Hero => t!("external.form.art.hero"),
            Self::Icon => t!("external.form.art.icon"),
        }
    }

    /// How a thumbnail is drawn: the shape the art has in the library.
    const fn thumb(self) -> (f32, f32) {
        match self {
            Self::Cover => (80., 120.),
            Self::Hero => (213., 69.),
            Self::Icon => (56., 56.),
        }
    }

    fn slot(self, art: &mut GameArt) -> &mut Option<String> {
        match self {
            Self::Cover => &mut art.cover,
            Self::Hero => &mut art.hero,
            Self::Icon => &mut art.icon,
        }
    }

    fn candidates(self, found: &Candidates) -> &[Candidate] {
        match self {
            Self::Cover => &found.covers,
            Self::Hero => &found.heroes,
            Self::Icon => &found.icons,
        }
    }
}

const KINDS: [ArtKind; 3] = [ArtKind::Cover, ArtKind::Hero, ArtKind::Icon];

impl GameForm {
    /// Fills whichever kinds are still empty with the best match, so a
    /// search alone is enough for a game that's easy to find.
    pub(super) fn search_art(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.error = Some(Text::new("external.form.errors.name_for_art"));
            cx.notify();
            return;
        }
        self.search = Search::Searching;
        self.error = None;
        // The same name finds its protonfixes entry too.
        if crate::launch::in_prefixes() {
            self.search_umu(cx);
        }
        cx.notify();
        self._work = Some(cx.spawn(async move |this, cx| {
            let found = crate::external::art::search(&name).await;
            let _ = this.update(cx, |this, cx| {
                for kind in KINDS {
                    let best = kind.candidates(&found).first().map(|candidate| candidate.url.clone());
                    let slot = kind.slot(&mut this.art);
                    if slot.is_none() {
                        *slot = best;
                    }
                }
                this.search = Search::Done(found);
                cx.notify();
            });
        }));
    }

    fn pick_image(&mut self, kind: ArtKind, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.art.pick_title"))
                .add_filter(t!("external.form.art.filter"), &["png", "jpg", "jpeg", "webp", "ico"])
                .pick_file()
                .await
            else {
                return;
            };
            let url = format!("file://{}", file.path().display());
            let _ = this.update(cx, |this, cx| {
                *kind.slot(&mut this.art) = Some(url);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn art_section(&self, cx: &Context<Self>) -> Div {
        let searching = matches!(self.search, Search::Searching);
        let found = match &self.search {
            Search::Done(found) => Some(found),
            Search::Idle | Search::Searching => None,
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mb_3()
                    .child(
                        button(
                            "external-search-art",
                            ButtonKind::Secondary,
                            if searching {
                                t!("external.form.art.searching")
                            } else {
                                t!("external.form.art.search")
                            },
                            searching,
                        )
                        .when(!searching, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.search_art(cx)))
                        }),
                    )
                    .child(theme::muted(t!("external.form.art.hint"))),
            )
            .children(found.map(|found| {
                div()
                    .flex()
                    .flex_col()
                    .when(found.is_empty(), |notes| {
                        notes.child(theme::muted(t!("external.form.art.none")))
                    })
                    .children(found.problems.iter().map(|problem| {
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(SharedString::from(problem))
                    }))
            }))
            .children(KINDS.map(|kind| self.art_row(kind, found, cx)))
    }

    /// One kind's current pick, then every candidate for it: click one to
    /// use it.
    fn art_row(&self, kind: ArtKind, found: Option<&Candidates>, cx: &Context<Self>) -> Div {
        let chosen = self.art_ref(kind).clone();
        let candidates = found.map(|found| kind.candidates(found)).unwrap_or_default();
        let thumbs = candidates.iter().enumerate().map(|(index, candidate)| {
            let selected = chosen.as_deref() == Some(candidate.url.as_str());
            let url = candidate.url.clone();
            thumb(kind, &candidate.url, selected)
                .id(gpui::ElementId::Name(format!("art-{}-{index}", kind as usize).into()))
                .cursor_pointer()
                .tooltip(tooltip(match candidate.source {
                    Source::SteamGridDb => "SteamGridDB",
                    Source::Steam => "Steam",
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    *kind.slot(&mut this.art) = Some(url.clone());
                    cx.notify();
                }))
                .into_any_element()
        });
        let current: AnyElement = match &chosen {
            Some(url) => thumb(kind, url, true).into_any_element(),
            None => empty_thumb(kind).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .mt_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mb_2()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(kind.label()),
                    )
                    .when(!candidates.is_empty(), |row| {
                        let count = candidates.len();
                        let label = if count == 1 {
                            t!("external.form.art.found.one", count = count)
                        } else {
                            t!("external.form.art.found.other", count = count)
                        };
                        row.child(theme::muted(label).text_size(px(12.)))
                    })
                    .child(
                        button(
                            ("art-file", kind as usize),
                            ButtonKind::Icon,
                            t!("external.form.art.file"),
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_image(kind, cx))),
                    )
                    .when(chosen.is_some(), |row| {
                        row.child(
                            button(
                                ("art-clear", kind as usize),
                                ButtonKind::Icon,
                                t!("external.form.art.clear"),
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                *kind.slot(&mut this.art) = None;
                                cx.notify();
                            })),
                        )
                    }),
            )
            .child(div().flex().gap(px(8.)).child(div().flex_none().child(current)).when(
                !candidates.is_empty(),
                |row| {
                    row.child(div().flex_none().w(px(1.)).bg(rgb(theme::LINE)))
                        .child(self.strip(kind, thumbs, cx))
                },
            ))
    }

    /// The candidates, in a row that scrolls sideways — by the wheel or a
    /// trackpad, or paged by the edge arrows, which only show while there
    /// is more that way.
    fn strip(&self, kind: ArtKind, thumbs: impl Iterator<Item = AnyElement>, cx: &Context<Self>) -> impl IntoElement {
        let scroll = &self.scrolls[kind as usize];
        // A pixel of slack either way: an eased scroll often lands a hair
        // off the exact end. A row not laid out yet reports no overflow,
        // so neither arrow shows until it has been measured.
        let offset = scroll.offset().x;
        let at_start = offset >= px(-1.);
        let at_end = -offset >= scroll.max_offset().x - px(1.);
        let arrow = |left: bool| {
            let (id, label, glyph, direction) = if left {
                ("art-arrow-left", t!("external.form.art.previous"), "‹", -1.)
            } else {
                ("art-arrow-right", t!("external.form.art.next"), "›", 1.)
            };
            div()
                .id((id, kind as usize))
                .absolute()
                .top_0()
                .bottom(px(8.))
                .w(px(40.))
                .when(left, |arrow| arrow.left_0())
                .when(!left, |arrow| arrow.right_0())
                .flex()
                .items_center()
                .justify_center()
                .bg(linear_gradient(
                    if left { 90. } else { 270. },
                    linear_color_stop(rgb(theme::BG_DEEP), 0.4),
                    linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 1.),
                ))
                .text_size(px(22.))
                .line_height(relative(1.))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .cursor_pointer()
                .hover(|arrow| arrow.text_color(rgb(theme::ACCENT_BLUE)))
                // Otherwise the thumbnail under the arrow takes the click too.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .tooltip(tooltip(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.page(kind, direction, cx);
                }))
                .child(glyph)
        };
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id(("art-candidates", kind as usize))
                    .flex()
                    .gap(px(8.))
                    .pb_2()
                    .overflow_x_scroll()
                    // A vertical wheel over the row scrolls the page past it
                    // instead of the row sideways.
                    .restrict_scroll_to_axis()
                    .track_scroll(scroll)
                    // The arrows depend on the offset, so a scroll has to
                    // re-render, not just repaint.
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .children(thumbs),
            )
            .when(!at_start, |strip| strip.child(arrow(true)))
            .when(!at_end, |strip| strip.child(arrow(false)))
    }

    /// Scrolls a row by most of its visible width, eased, the same way the
    /// library's shelves page — a click always brings in a fresh set.
    fn page(&mut self, kind: ArtKind, direction: f32, cx: &mut Context<Self>) {
        let handle = self.scrolls[kind as usize].clone();
        let start = handle.offset();
        let width = handle.bounds().size.width;
        // GPUI's offset grows negative as the row scrolls right.
        let target = (start.x - width * (0.85 * direction))
            .min(px(0.))
            .max(-handle.max_offset().x);
        self.paging = Some(cx.spawn(async move |this, cx| {
            const FRAMES: u32 = 15;
            for frame in 1..=FRAMES {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let t = frame as f32 / FRAMES as f32;
                let eased = 1. - (1. - t).powi(3);
                handle.set_offset(point(start.x + (target - start.x) * eased, start.y));
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    fn art_ref(&self, kind: ArtKind) -> &Option<String> {
        match kind {
            ArtKind::Cover => &self.art.cover,
            ArtKind::Hero => &self.art.hero,
            ArtKind::Icon => &self.art.icon,
        }
    }
}

fn thumb(kind: ArtKind, url: &str, selected: bool) -> Div {
    let (width, height) = kind.thumb();
    div()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .rounded(px(theme::RADIUS_SM))
        .overflow_hidden()
        .border_2()
        .border_color(if selected {
            rgb(theme::ACCENT_BLUE)
        } else {
            rgb(theme::LINE)
        })
        .bg(rgb(theme::BG_PANEL))
        .child(
            img(crate::art::source(url, gpui::size(width, height)))
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
}

fn empty_thumb(kind: ArtKind) -> Div {
    let (width, height) = kind.thumb();
    div()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_dashed()
        .border_color(rgb(theme::LINE))
        .text_size(px(11.))
        .text_color(rgb(theme::TEXT_MUTED))
        .child(t!("external.form.art.empty"))
}
