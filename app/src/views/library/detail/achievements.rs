use gpui::{AnyElement, Context, Div, FontWeight, ObjectFit, WeakEntity, canvas, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::DetailView;
use crate::theme;
use crate::views::library::data::{Achievement, PollStatus, UnlockedAt};
use crate::views::library::fmt;

/// `.achv-grid`'s `repeat(auto-fill, minmax(260px, 1fr))` and its gap.
const ACHV_MIN_COLUMN: f32 = 260.;
const ACHV_GAP: f32 = 10.;

impl DetailView {
    /// The cache being empty is ambiguous on its own — still loading, this
    /// game genuinely has no achievements, or the poll failed. `poll_status`
    /// (the live poll that just ran, or is running, for this exact game)
    /// disambiguates it instead of always pointing at a CLI command.
    pub(super) fn achievements_tab(
        &self,
        achievements: Option<&Result<Vec<Achievement>, String>>,
        poll_status: &PollStatus,
        cx: &Context<Self>,
    ) -> AnyElement {
        let status_text = |text: String| div().mt(px(20.)).mx(px(32.)).child(theme::muted(text));
        let status_error = |text: String| div().mt(px(20.)).mx(px(32.)).child(theme::error(text));
        match achievements {
            Some(Ok(list)) if !list.is_empty() => return self.achievement_grid(list, cx),
            Some(Err(err)) => {
                return status_error(t!("library.detail.achievements.cache_error", error = err).into_owned())
                    .into_any_element();
            }
            Some(Ok(_)) | None => {}
        }
        match poll_status {
            PollStatus::Polling => status_text(t!("library.detail.achievements.polling").into_owned()),
            PollStatus::Done => div()
                .mt(px(20.))
                .mx(px(32.))
                .child(theme::empty(t!("library.detail.achievements.none"))),
            PollStatus::Failed(err) => {
                status_error(t!("library.detail.achievements.poll_failed", error = err).into_owned())
            }
            PollStatus::Idle => status_text(t!("common.loading").into_owned()),
        }
        .into_any_element()
    }

    /// `auto-fill` has no GPUI equivalent, so the grid measures its own
    /// width each frame and derives the column count the way CSS would:
    /// as many 260px-minimum columns as fit, sharing what's left over.
    fn achievement_grid(&self, achievements: &[Achievement], cx: &Context<Self>) -> AnyElement {
        let width = f32::from(self.grid_width);
        let columns = ((width + ACHV_GAP) / (ACHV_MIN_COLUMN + ACHV_GAP)).floor().max(1.) as u16;
        let this = cx.entity().downgrade();
        div()
            .relative()
            .mt(px(20.))
            .mx(px(32.))
            .child(measure_width(this))
            .child(
                div()
                    .grid()
                    .grid_cols(columns)
                    .gap(px(ACHV_GAP))
                    .children(achievements.iter().map(achievement_tile)),
            )
            .into_any_element()
    }
}

fn measure_width(this: WeakEntity<DetailView>) -> impl IntoElement {
    canvas(
        move |bounds, _, cx| {
            let width = bounds.size.width;
            let unchanged = this.read_with(cx, |this, _| this.grid_width == width).unwrap_or(true);
            if unchanged {
                return;
            }
            // Deferred because a notify during prepaint is dropped: the
            // window is mid-draw, so it would never schedule the re-layout
            // with the new column count.
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| {
                    this.grid_width = width;
                    cx.notify();
                });
            });
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn achievement_tile(achievement: &Achievement) -> Div {
    let unlocked_at = achievement.unlocked_at.as_ref().map(|at| match at {
        UnlockedAt::At(at) => fmt::date_time(*at),
        UnlockedAt::Raw(raw) => raw.clone(),
    });
    let achievement = &achievement.cached;
    let unlocked = achievement.unlocked;
    let icon = div()
        .size(px(40.))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .bg(rgb(theme::BG_PANEL_LIGHT))
        .overflow_hidden()
        .children(icon_for(achievement).map(|url| {
            img(crate::art::source(url, gpui::size(40., 40.)))
                .size_full()
                .rounded(px(theme::RADIUS_SM))
                .object_fit(ObjectFit::Cover)
                .grayscale(!unlocked)
        }));
    let info = div()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(12.5))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(achievement.name.clone()),
        )
        .children(achievement.description.clone().map(|desc| {
            div()
                .mt(px(2.))
                .text_size(px(11.))
                .text_color(rgb(theme::TEXT_MUTED))
                .truncate()
                .child(desc)
        }))
        .children(unlocked_at.map(|at| {
            theme::mono()
                .mt_1()
                .text_size(px(10.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(at)
        }));
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w_0()
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(if unlocked {
            theme::alpha(theme::ACCENT_GOLD, 0.4)
        } else {
            rgb(theme::LINE).into()
        })
        .rounded(px(theme::RADIUS))
        .px_3()
        .py(px(10.))
        .when(!unlocked, |tile| tile.opacity(0.5))
        .child(icon)
        .child(info)
}

fn icon_for(achievement: &decanter_core::CachedAchievement) -> Option<&str> {
    if achievement.unlocked {
        achievement
            .icon_unlocked_url
            .as_deref()
            .or(achievement.icon_locked_url.as_deref())
    } else {
        achievement
            .icon_locked_url
            .as_deref()
            .or(achievement.icon_unlocked_url.as_deref())
    }
}
