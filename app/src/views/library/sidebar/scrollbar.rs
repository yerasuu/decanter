use gpui::{Context, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Window, div, point, prelude::*, px};

use super::SidebarView;
use crate::theme;

/// The scrollbar thumb's place, all in pixels: offset from the top of the
/// list, its length, how far it can travel, and the list's own furthest
/// scroll offset.
struct Thumb {
    top: Pixels,
    length: Pixels,
    travel: Pixels,
    max_scroll: Pixels,
}

impl SidebarView {
    fn thumb(&self) -> Option<Thumb> {
        let viewport = self.list.viewport_bounds().size.height;
        let max_scroll = self.list.max_offset_for_scrollbar().y;
        if viewport <= px(0.) || max_scroll <= px(0.) {
            return None;
        }
        let length = (viewport * (viewport / (viewport + max_scroll))).max(px(24.));
        let travel = viewport - length;
        let scrolled = -self.list.scroll_px_offset_for_scrollbar().y;
        Some(Thumb {
            top: travel * (scrolled / max_scroll).clamp(0., 1.),
            length,
            travel,
            max_scroll,
        })
    }

    /// Scrolls so the thumb's grabbed point sits under the pointer.
    fn scroll_to_pointer(&self, pointer_y: Pixels, grab: Pixels, thumb: &Thumb) {
        let top = (pointer_y - self.list.viewport_bounds().origin.y - grab)
            .max(px(0.))
            .min(thumb.travel);
        let fraction = if thumb.travel > px(0.) { top / thumb.travel } else { 0. };
        self.list
            .set_offset_from_scrollbar(point(px(0.), -(thumb.max_scroll * fraction)));
    }

    /// GPUI draws no scrollbar of its own; without one a library of
    /// hundreds of games gave no sign the list went on, or how far.
    pub(super) fn scrollbar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let Some(thumb) = self.thumb() else {
            return None;
        };
        let dragging = self.thumb_grab.is_some();
        Some(
            div()
                .id("sidebar-scrollbar")
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(px(10.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        let Some(thumb) = this.thumb() else {
                            return;
                        };
                        let on_thumb = event.position.y - this.list.viewport_bounds().origin.y - thumb.top;
                        // Pressing the track rather than the thumb jumps the
                        // thumb's middle to the pointer, then drags from there.
                        let grab = if on_thumb >= px(0.) && on_thumb <= thumb.length {
                            on_thumb
                        } else {
                            thumb.length / 2.
                        };
                        this.list.scrollbar_drag_started();
                        this.scroll_to_pointer(event.position.y, grab, &thumb);
                        this.thumb_grab = Some(grab);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top(thumb.top)
                        .right(px(2.))
                        .w(px(5.))
                        .h(thumb.length)
                        .rounded_full()
                        .bg(theme::alpha(theme::TEXT_MUTED, if dragging { 0.7 } else { 0.4 }))
                        .hover(|thumb| thumb.bg(theme::alpha(theme::TEXT_MUTED, 0.7))),
                ),
        )
    }

    /// Follows the pointer anywhere over the sidebar while the thumb is
    /// held, and lets go once the button is up — a release outside the
    /// window never reaches a mouse-up handler here.
    pub(super) fn drag_thumb(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(grab) = self.thumb_grab else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.thumb_grab = None;
            self.list.scrollbar_drag_ended();
            cx.notify();
            return;
        }
        if let Some(thumb) = self.thumb() {
            self.scroll_to_pointer(event.position.y, grab, &thumb);
        }
        cx.notify();
    }
}
