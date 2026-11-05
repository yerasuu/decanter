//! Keeps the focused control in view: GPUI scrolls a container for the wheel and for drags, never
//! for the focus, so Tab and the arrows could walk to a button below the fold of a page.
//!
//! [`scrolling`] wraps a scroll container and [`reveal`] wraps a block of controls inside it (a
//! row, a card); once the focus lands in the block, the nearest container around it scrolls just
//! far enough to show the block whole. The pair finds each other while the frame is prepainted,
//! so nothing is plumbed through the screens in between.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, FocusHandle, Global, GlobalElementId, InspectorElementId,
    InteractiveElement, IntoElement, LayoutId, Pixels, ScrollHandle, StatefulInteractiveElement, Window, point, px,
};

/// Room left around a revealed block so the focus ring around its control is not clipped.
const MARGIN: Pixels = px(8.);

/// The scroll containers whose children are being prepainted, innermost last.
#[derive(Default)]
struct Containers(Vec<ScrollHandle>);

impl Global for Containers {}

/// A scroll container that [`reveal`]ed blocks inside it can scroll. `element` must have an id and
/// its overflow set.
pub fn scrolling<E: StatefulInteractiveElement + Element>(element: E) -> Scrolling<E> {
    Scrolling {
        id: Element::id(&element),
        element: Some(element),
    }
}

/// A block of controls that scrolls the container around it to show itself whenever the keyboard
/// moves the focus into it. `element` must have an id; it becomes focusable (but not a tab stop)
/// so that the focus inside it can be told apart from the focus elsewhere.
pub fn reveal<E: InteractiveElement + Element>(element: E) -> Reveal<E> {
    Reveal {
        id: Element::id(&element),
        element: Some(element),
        tab_stop: false,
    }
}

/// [`reveal`] for a control that is itself a tab stop: it keeps being one.
pub fn reveal_stop<E: InteractiveElement + Element>(element: E) -> Reveal<E> {
    Reveal {
        tab_stop: true,
        ..reveal(element)
    }
}

pub struct Scrolling<E> {
    /// Read before the element is moved into the child, because `id` is asked again in every
    /// phase and GPUI checks that the id stack it pushes stays the same across them.
    id: Option<ElementId>,
    element: Option<E>,
}

pub struct ScrollingLayout {
    child: Option<AnyElement>,
    scroll: Option<ScrollHandle>,
}

impl<E: StatefulInteractiveElement + Element> Element for Scrolling<E> {
    type RequestLayoutState = ScrollingLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ScrollingLayout) {
        let scroll = id.map(|id| {
            window.with_element_state(id, |scroll: Option<ScrollHandle>, _| {
                let scroll = scroll.unwrap_or_default();
                (scroll.clone(), scroll)
            })
        });
        let mut child = self.element.take().map(|element| match &scroll {
            Some(scroll) => element.track_scroll(scroll).into_any_element(),
            None => element.into_any_element(),
        });
        let layout = match child.as_mut() {
            Some(child) => child.request_layout(window, cx),
            None => window.request_layout(Default::default(), None, cx),
        };
        (layout, ScrollingLayout { child, scroll })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        state: &mut ScrollingLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(child) = state.child.as_mut() else { return };
        if let Some(scroll) = &state.scroll {
            cx.default_global::<Containers>().0.push(scroll.clone());
        }
        child.prepaint(window, cx);
        if state.scroll.is_some() {
            cx.default_global::<Containers>().0.pop();
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        state: &mut ScrollingLayout,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = state.child.as_mut() {
            child.paint(window, cx);
        }
    }
}

impl<E: StatefulInteractiveElement + Element> IntoElement for Scrolling<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

pub struct Reveal<E> {
    id: Option<ElementId>,
    element: Option<E>,
    tab_stop: bool,
}

#[derive(Clone)]
struct Anchor {
    focus: FocusHandle,
    /// The focus this block already scrolled for, so a block is brought into view once rather
    /// than pulled back on every scroll of the wheel.
    revealed: Rc<RefCell<Option<FocusHandle>>>,
}

pub struct RevealLayout {
    child: Option<AnyElement>,
    anchor: Option<Anchor>,
}

impl<E: InteractiveElement + Element> Element for Reveal<E> {
    type RequestLayoutState = RevealLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, RevealLayout) {
        let tab_stop = self.tab_stop;
        let anchor = id.map(|id| {
            window.with_element_state(id, |anchor: Option<Anchor>, _| {
                let anchor = anchor.unwrap_or_else(|| Anchor {
                    focus: cx.focus_handle().tab_stop(tab_stop),
                    revealed: Rc::default(),
                });
                (anchor.clone(), anchor)
            })
        });
        let mut child = self.element.take().map(|element| match &anchor {
            Some(anchor) => element.track_focus(&anchor.focus).into_any_element(),
            None => element.into_any_element(),
        });
        let layout = match child.as_mut() {
            Some(child) => child.request_layout(window, cx),
            None => window.request_layout(Default::default(), None, cx),
        };
        (layout, RevealLayout { child, anchor })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut RevealLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = state.child.as_mut() {
            child.prepaint(window, cx);
        }
        let Some(anchor) = &state.anchor else { return };
        let focused = window.focused(cx).filter(|_| anchor.focus.contains_focused(window, cx));
        if anchor.revealed.replace(focused.clone()) == focused || focused.is_none() {
            return;
        }
        let Some(scroll) = cx.default_global::<Containers>().0.last().cloned() else {
            return;
        };
        scroll_to(&scroll, bounds, window);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        state: &mut RevealLayout,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = state.child.as_mut() {
            child.paint(window, cx);
        }
    }
}

impl<E: InteractiveElement + Element> IntoElement for Reveal<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Moves `scroll` the least that shows `block`, or its top when it is taller than the viewport.
/// Applied before the next frame is drawn, because the container already laid out this one.
fn scroll_to(scroll: &ScrollHandle, block: Bounds<Pixels>, window: &mut Window) {
    let view = scroll.bounds();
    let shift = if block.size.height > view.size.height || block.top() < view.top() + MARGIN {
        view.top() + MARGIN - block.top()
    } else if block.bottom() > view.bottom() - MARGIN {
        view.bottom() - MARGIN - block.bottom()
    } else {
        return;
    };
    let scroll = scroll.clone();
    window.on_next_frame(move |window, _| {
        let offset = scroll.offset();
        let y = (offset.y + shift).clamp(-scroll.max_offset().y, px(0.));
        scroll.set_offset(point(offset.x, y));
        window.refresh();
    });
}
