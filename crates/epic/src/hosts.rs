// SPDX-License-Identifier: GPL-3.0-or-later
//! Every Epic host decanter may call, in one table that a release changes as a whole.
//!
//! The names are the ones the official launcher's client uses (spec, section 2.1). Their numeric
//! and region suffixes (`prod03`, `prod06`, `ecomprod02`, `use1a`, `beee`) are what drifts when
//! Epic moves a service (spec, section 13.2), so nothing outside this module spells a host, and a
//! route that stops answering reads as [`crate::CallError::Moved`] or
//! [`crate::CallError::Unreachable`], never as a refused session.

use crate::identity::{LAUNCHER_USER_AGENT, STORE_USER_AGENT};

/// What an Epic host is for. The host name and the user agent come from the table, never from
/// the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Host {
    /// Accounts and their sign-in: token, verify, exchange code, external auths.
    Accounts,
    /// The launcher service: assets and game manifests.
    Launcher,
    Entitlements,
    /// Per-item catalog metadata.
    Catalog,
    /// Ownership tokens of titles that ask for one at launch.
    Ecommerce,
    CloudSaves,
    /// Library items, those without an asset included.
    Library,
    /// The store's `/graphql` endpoint: achievements.
    Store,
    /// Tickets for the EOS helper content.
    Artifact,
}

struct Entry {
    name: &'static str,
    user_agent: &'static str,
}

impl Host {
    const fn entry(self) -> Entry {
        let (name, user_agent) = match self {
            Self::Accounts => ("account-public-service-prod03.ol.epicgames.com", LAUNCHER_USER_AGENT),
            Self::Launcher => ("launcher-public-service-prod06.ol.epicgames.com", LAUNCHER_USER_AGENT),
            Self::Entitlements => (
                "entitlement-public-service-prod08.ol.epicgames.com",
                LAUNCHER_USER_AGENT,
            ),
            Self::Catalog => ("catalog-public-service-prod06.ol.epicgames.com", LAUNCHER_USER_AGENT),
            Self::Ecommerce => (
                "ecommerceintegration-public-service-ecomprod02.ol.epicgames.com",
                LAUNCHER_USER_AGENT,
            ),
            Self::CloudSaves => (
                "datastorage-public-service-liveegs.live.use1a.on.epicgames.com",
                LAUNCHER_USER_AGENT,
            ),
            Self::Library => ("library-service.live.use1a.on.epicgames.com", LAUNCHER_USER_AGENT),
            // The launcher sends its store calls under a newer version than the rest (spec, 2.2).
            Self::Store => ("launcher.store.epicgames.com", STORE_USER_AGENT),
            Self::Artifact => (
                "artifact-public-service-prod.beee.live.use1a.on.epicgames.com",
                LAUNCHER_USER_AGENT,
            ),
        };
        Entry { name, user_agent }
    }

    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.entry().name
    }

    /// The User-Agent every call to this host carries, so that decanter's requests look like the
    /// launcher whose client id they bear.
    #[inline]
    #[must_use]
    pub const fn user_agent(self) -> &'static str {
        self.entry().user_agent
    }

    /// The HTTPS address of `path` on this host; `path` must start with `/`.
    pub(crate) fn url(self, path: &str) -> String {
        format!("https://{}{path}", self.name())
    }
}
