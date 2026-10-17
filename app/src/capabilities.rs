//! The capabilities table as a GPUI global, and the helpers views gate a
//! feature with, so each one is shown, marked as being tested or left out
//! the same way everywhere.

use decanter_core::capabilities::{Capabilities, Feature, Reason, Source, Support};
use decanter_ps::bbport::HostFeatures;
use gpui::{AnyElement, App, Div, Global, IntoElement, ParentElement as _, SharedString, Styled as _, div};
use rust_i18n::t;

use crate::self_update::{self, Availability};
use crate::{platform, theme};

pub struct Caps {
    /// decanter's table and its own checks, which a new report from psport
    /// starts again from.
    own: Capabilities,
    /// `own` with what psport last reported about itself.
    current: Capabilities,
}

impl Global for Caps {}

impl Caps {
    /// The table with what can be answered on the spot, then the tray
    /// host's answer once the session bus gives it.
    pub fn init(cx: &mut App) {
        let mut capabilities = Capabilities::detect();
        match self_update::availability() {
            Availability::AppImage(_) => {}
            Availability::NoKey => capabilities.restrict(Feature::SelfUpdate, Reason::NeedsSetup("release key")),
            Availability::Elsewhere => capabilities.restrict(Feature::SelfUpdate, Reason::NotOnThisSystem),
        }
        let ask_tray_host = capabilities.is_offered(Feature::Tray);
        cx.set_global(Self {
            current: capabilities.clone(),
            own: capabilities,
        });
        if ask_tray_host {
            cx.spawn(async |async_cx| {
                if !platform::status_notifier_host().await {
                    async_cx.update(|app| {
                        Self::restrict(
                            app,
                            &[Feature::Tray, Feature::CloseToTray],
                            &Reason::NeedsSetup("StatusNotifier host"),
                        );
                    });
                }
            })
            .detach();
        }
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Redraws the windows, since what they show may have just gone.
    fn restrict(cx: &mut App, features: &[Feature], reason: &Reason) {
        let caps = cx.global_mut::<Self>();
        for feature in features {
            caps.own.restrict(*feature, reason.clone());
            caps.current.restrict(*feature, reason.clone());
        }
        cx.refresh_windows();
    }

    /// Takes psport's latest report in place of the one before, which a
    /// newer build may have outgrown; `None`, a port that doesn't report,
    /// leaves decanter's own table.
    pub fn take_psport_report(cx: &mut App, report: Option<&HostFeatures>) {
        let caps = cx.global_mut::<Self>();
        caps.current = caps.own.clone();
        if let Some(report) = report {
            report.merge_into(&mut caps.current);
        }
        cx.refresh_windows();
    }

    pub fn is_offered(&self, feature: Feature) -> bool {
        self.current.is_offered(feature)
    }

    pub fn support(&self, feature: Feature) -> &Support {
        self.current.support(feature)
    }

    /// Why `feature` isn't offered, for a view that leaves out a whole pane
    /// rather than one element.
    pub fn reason(&self, feature: Feature) -> Option<SharedString> {
        match self.support(feature) {
            Support::Unavailable(reason) => Some(reason_text(reason)),
            Support::Available | Support::Testing => None,
        }
    }

    /// `element`, marked when it is being tested; nothing when unavailable.
    pub fn gate(&self, feature: Feature, element: impl IntoElement) -> Option<AnyElement> {
        match self.support(feature) {
            Support::Available => Some(element.into_any_element()),
            Support::Testing => Some(with_testing_badge(element)),
            Support::Unavailable(_) => None,
        }
    }

    /// [`Self::gate`], with why it's unavailable in its place, muted.
    pub fn gate_or_reason(&self, feature: Feature, element: impl IntoElement) -> AnyElement {
        self.gate_or(feature, element, self.reason(feature).unwrap_or_default())
    }

    /// [`Self::gate`], with `instead` in its place, muted, when unavailable.
    pub fn gate_or(&self, feature: Feature, element: impl IntoElement, instead: impl Into<SharedString>) -> AnyElement {
        match self.support(feature) {
            Support::Available => element.into_any_element(),
            Support::Testing => with_testing_badge(element),
            Support::Unavailable(_) => theme::muted(instead).into_any_element(),
        }
    }
}

/// For a row that has room for it beside its own label, such as a menu item.
pub fn testing_badge() -> Div {
    theme::badge(t!("common.testing"), false)
}

fn with_testing_badge(element: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(element)
        .child(testing_badge())
        .into_any_element()
}

fn reason_text(reason: &Reason) -> SharedString {
    match reason {
        Reason::NotOnThisSystem => t!("common.unavailable_here"),
        Reason::NeedsSetup(what) => t!("common.needs_setup", what = what),
        Reason::ReportedBy(Source::Psport, why) => t!("common.reported_by_ps", why = why),
    }
    .into()
}
