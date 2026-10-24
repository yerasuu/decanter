//! The capabilities table as a GPUI global, and the helpers views gate a
//! feature with, so each one is shown, marked as being tested or left out
//! the same way everywhere.

use std::borrow::Cow;

use decanter_core::capabilities::{Capabilities, Feature, Reason, Source, Support};
use decanter_ps::bbport::HostFeatures;
use gpui::{AnyElement, App, Div, Global, IntoElement, ParentElement as _, SharedString, Styled as _, div};
use rust_i18n::t;

use crate::self_update::{self, Availability};
use crate::{platform, theme};

// What decanter itself finds missing, in technical English for the log as
// `Reason::NeedsSetup` asks; `setup_text` words them for the UI.
const RELEASE_KEY: &str = "release key";
const TRAY_HOST: &str = "StatusNotifier host";
const PSPORT_MENU_BINDING: &str = "psport build listing port_menu_binding";
const PSPORT_KEY_STICK: &str = "psport build listing key_stick";
const PSPORT_ONLINE: &str = "psport build listing online_module";

pub struct Caps {
    /// decanter's table and its own checks, which a new report from psport
    /// starts again from.
    own: Capabilities,
    /// `own` with what psport last reported about itself.
    current: Capabilities,
    /// The setting options psport last reported unsuitable for this machine, by feature row id.
    ps_unavailable_options: Vec<String>,
}

impl Global for Caps {}

impl Caps {
    /// The table with what can be answered on the spot, then the tray
    /// host's answer once the session bus gives it.
    pub fn init(cx: &mut App) {
        let mut capabilities = Capabilities::detect();
        match self_update::availability() {
            Availability::AppImage(_) => {}
            Availability::NoKey => capabilities.restrict(Feature::SelfUpdate, Reason::NeedsSetup(RELEASE_KEY)),
            Availability::Elsewhere => capabilities.restrict(Feature::SelfUpdate, Reason::NotOnThisSystem),
        }
        let ask_tray_host = capabilities.is_offered(Feature::Tray);
        let mut current = capabilities.clone();
        without_psport_report(&mut current);
        cx.set_global(Self {
            current,
            own: capabilities,
            ps_unavailable_options: Vec::new(),
        });
        if ask_tray_host {
            cx.spawn(async |async_cx| {
                if !platform::status_notifier_host().await {
                    async_cx.update(|app| {
                        Self::restrict(
                            app,
                            &[Feature::Tray, Feature::CloseToTray],
                            &Reason::NeedsSetup(TRAY_HOST),
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
        caps.ps_unavailable_options = report.map(HostFeatures::unavailable_options).unwrap_or_default();
        match report {
            Some(report) => report.merge_into(&mut caps.current),
            None => without_psport_report(&mut caps.current),
        }
        cx.refresh_windows();
    }

    /// The PS settings, or with an option value single options of them, that psport last reported
    /// unsuitable for this machine ([`decanter_ps::bbport::OPTION_FEATURES`]); none without a report.
    pub fn ps_hidden_options(&self) -> Vec<(&'static str, Option<&'static str>)> {
        decanter_ps::bbport::OPTION_FEATURES
            .iter()
            .filter(|&&(_, _, row)| self.ps_unavailable_options.iter().any(|id| id == row))
            .map(|&(key, value, _)| (key, value))
            .collect()
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

    /// `element` whatever the support, marked when it is being tested: for what stays shown and
    /// says why itself when unavailable.
    pub fn mark(&self, feature: Feature, element: impl IntoElement) -> AnyElement {
        match self.support(feature) {
            Support::Testing => with_testing_badge(element),
            Support::Available | Support::Unavailable(_) => element.into_any_element(),
        }
    }

    /// `element`, marked when it is being tested, with why it's unavailable in its place, muted.
    pub fn gate_or_reason(&self, feature: Feature, element: impl IntoElement) -> AnyElement {
        self.gate_or(feature, element, self.reason(feature).unwrap_or_default())
    }

    /// `element`, marked when it is being tested, with `instead` in its place, muted, when unavailable.
    pub fn gate_or(&self, feature: Feature, element: impl IntoElement, instead: impl Into<SharedString>) -> AnyElement {
        match self.support(feature) {
            Support::Available => element.into_any_element(),
            Support::Testing => with_testing_badge(element),
            Support::Unavailable(_) => theme::muted(instead).into_any_element(),
        }
    }
}

/// What only a psport build's own report can offer stays off until one
/// arrives, since builds from before a feature silently ignore it.
fn without_psport_report(capabilities: &mut Capabilities) {
    capabilities.restrict(Feature::PsMenuBinding, Reason::NeedsSetup(PSPORT_MENU_BINDING));
    capabilities.restrict(Feature::PsKeyStick, Reason::NeedsSetup(PSPORT_KEY_STICK));
    capabilities.restrict(Feature::PsOnline, Reason::NeedsSetup(PSPORT_ONLINE));
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
        Reason::NeedsSetup(what) => t!("common.needs_setup", what = setup_text(what)),
        Reason::ReportedBy(Source::Psport, why) => t!("common.reported_by_ps", why = why),
        Reason::NotPortedYet(Source::Psport) => t!("common.not_ported_yet_ps", system = system_name()),
    }
    .into()
}

/// This system's name as its users know it.
fn system_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// `what` for the UI: decanter's own needs translated, while the names decanter-core gives (a
/// program's own name) read the same in every language.
fn setup_text(what: &'static str) -> Cow<'static, str> {
    match what {
        RELEASE_KEY => t!("common.setup.release_key"),
        TRAY_HOST => t!("common.setup.tray_host"),
        PSPORT_MENU_BINDING => t!("common.setup.psport_menu_binding"),
        PSPORT_KEY_STICK => t!("common.setup.psport_key_stick"),
        PSPORT_ONLINE => t!("common.setup.psport_online"),
        _ => Cow::Borrowed(what),
    }
}
