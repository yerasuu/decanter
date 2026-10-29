//! What decanter can do on this system, and in which state: the one place
//! the UI and the CLI ask before offering a feature (decision #538).
//!
//! Each OS's table in [`crate::platform`] says what it supports at all;
//! checks only this machine can answer then take away what isn't there
//! (a systemd user manager, a tray host, a writable `AppImage`), and later
//! what psport itself reports. Nothing here ever turns a feature on that
//! the table leaves off.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "features are grouped by what they belong to (games, desktop, PS, tools), not alphabetically"
)]

use crate::{game_tools, platform, scope};

/// A feature the UI shows or hides as a whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    Play,
    Proton,
    Prefixes,
    ExternalInstaller,
    GalaxyHook,
    Tray,
    CloseToTray,
    /// Closing the running game from a pad chord or the desktop's shortcut.
    PadClose,
    DesktopEntries,
    /// One systemd scope per game.
    Scopes,
    SelfUpdate,
    SteamExport,
    PsSection,
    /// Getting, building and updating a port in the library: its checkout,
    /// the toolchain (nix or the host's tools), its prebuilt and released
    /// builds, and what runs inside them (the Vulkan validation layer, FSR
    /// 4.1.1's recording). Where psport.exe runs directly (Windows), building
    /// psport's branch with the toolchain decanter pins instead.
    PsBuilds,
    PsDownloads,
    PsPlay,
    /// A key or pad button bound to the port's own menu: only psport
    /// builds that list it among their features read the binding.
    PsMenuBinding,
    /// psport.ini's keyboard stick settings: only psport builds that list
    /// them among their features read them.
    PsKeyStick,
    /// Per-game frame generation: only builds that list `frame_gen_fsr` among their features have it.
    PsFrameGen,
    /// Online play through psport's online module: only builds that list
    /// it among their features have one.
    PsOnline,
    MangoHud,
    GameMode,
    /// Raven's own section: building Raven here and running Windows
    /// programs with it. Experimental wherever it is offered.
    Raven,
}

impl Feature {
    pub const ALL: [Self; 23] = [
        Self::Play,
        Self::Proton,
        Self::Prefixes,
        Self::ExternalInstaller,
        Self::GalaxyHook,
        Self::Tray,
        Self::CloseToTray,
        Self::PadClose,
        Self::DesktopEntries,
        Self::Scopes,
        Self::SelfUpdate,
        Self::SteamExport,
        Self::PsSection,
        Self::PsBuilds,
        Self::PsDownloads,
        Self::PsPlay,
        Self::PsMenuBinding,
        Self::PsKeyStick,
        Self::PsFrameGen,
        Self::PsOnline,
        Self::MangoHud,
        Self::GameMode,
        Self::Raven,
    ];
}

#[derive(Clone, Debug)]
pub enum Support {
    Available,
    /// Offered, but new on this system: the UI marks it as being tested.
    Testing,
    Unavailable(Reason),
}

impl Support {
    /// Offered at all, tested or not.
    #[must_use]
    #[inline]
    pub const fn is_offered(&self) -> bool {
        matches!(self, Self::Available | Self::Testing)
    }
}

#[derive(Clone, Debug)]
pub enum Reason {
    NotOnThisSystem,
    /// Something this system lacks and the user could add, named in
    /// technical English.
    NeedsSetup(&'static str),
    /// What a component said about itself, in its own words.
    ReportedBy(Source, String),
    /// A component that hasn't been ported to this system yet: what needs
    /// it is shown with this, rather than left out.
    NotPortedYet(Source),
    /// Not compiled on this system yet: the component's published release is downloaded ready-made
    /// instead.
    DownloadsInstead(Source),
}

#[derive(Clone, Copy, Debug)]
pub enum Source {
    Psport,
}

/// What a feature missing from the table, which can't happen, reads as.
static NOT_HERE: Support = Support::Unavailable(Reason::NotOnThisSystem);

#[derive(Clone)]
pub struct Capabilities {
    /// Every feature once, in [`Feature::ALL`]'s order.
    states: [(Feature, Support); Feature::ALL.len()],
}

impl Capabilities {
    /// This OS's table, less what this machine can be seen to lack without
    /// asking anyone over a bus.
    #[must_use]
    #[inline]
    pub fn detect() -> Self {
        let mut capabilities = Self {
            states: Feature::ALL.map(|feature| (feature, platform::support(feature))),
        };
        if scope::systemd_run().is_none() {
            capabilities.restrict(Feature::Scopes, Reason::NotOnThisSystem);
        }
        if !game_tools::mangohud_installed() {
            capabilities.restrict(Feature::MangoHud, Reason::NeedsSetup("MangoHud"));
        }
        if game_tools::gamemoded().is_none() {
            capabilities.restrict(Feature::GameMode, Reason::NeedsSetup("GameMode"));
        }
        capabilities
    }

    #[must_use]
    #[inline]
    pub fn support(&self, feature: Feature) -> &Support {
        self.states
            .iter()
            .find(|state| state.0 == feature)
            .map_or(&NOT_HERE, |state| &state.1)
    }

    #[must_use]
    #[inline]
    pub fn is_offered(&self, feature: Feature) -> bool {
        self.support(feature).is_offered()
    }

    /// Takes `feature` away for `reason`, unless it already was: the first
    /// reason, usually the table's, is the one worth showing.
    #[inline]
    pub fn restrict(&mut self, feature: Feature, reason: Reason) {
        if let Some(state) = self.states.iter_mut().find(|state| state.0 == feature)
            && state.1.is_offered()
        {
            tracing::info!(?feature, ?reason, "feature unavailable on this machine");
            state.1 = Support::Unavailable(reason);
        }
    }

    /// Marks an available feature as being tested, as when the component
    /// behind it says part of it is experimental here.
    #[inline]
    pub fn mark_testing(&mut self, feature: Feature) {
        if let Some(state) = self.states.iter_mut().find(|state| state.0 == feature)
            && matches!(state.1, Support::Available)
        {
            tracing::info!(?feature, "feature marked as being tested");
            state.1 = Support::Testing;
        }
    }

    /// What `source` says it can't do, each with its own explanation.
    #[inline]
    pub fn merge_reported<I>(&mut self, source: Source, missing: I)
    where
        I: IntoIterator<Item = (Feature, String)>,
    {
        for (feature, why) in missing {
            self.restrict(feature, Reason::ReportedBy(source, why));
        }
    }
}
