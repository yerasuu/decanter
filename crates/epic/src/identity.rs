// SPDX-License-Identifier: GPL-3.0-or-later
//! Who decanter says it is to Epic, in one place so that the values can change together.
//!
//! Epic publishes no API and no third-party client policy for its launcher services, so decanter
//! signs in as the official Epic Games Launcher: its login client and its user agents (spec,
//! section 2.2). The values are pinned here rather than fetched at run time, so a rotation by Epic
//! needs a decanter release (decision D3 of docs/epic-legendary-integration-analysis.md). The
//! client id and secret are the only values taken from legendary-gl 0.21.1 (`api/egs.py`), and
//! UNVERIFIED that Epic still accepts them. Presenting itself as that launcher is the account risk
//! the consent notice tells the user about before the first login.
//!
//! Downloads from the CDN carry the launcher's `EpicGamesLauncher/…` user agent instead of these
//! (spec, section 2.2).

/// The launcher's login client id. Not a secret: the browser login sends it in the clear.
pub(crate) const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
/// The launcher's login client secret, the password of the token endpoint's HTTP Basic auth.
pub(crate) const CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";

/// The User-Agent of the launcher's calls to its services.
pub(crate) const LAUNCHER_USER_AGENT: &str =
    "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";
/// The User-Agent of the launcher's calls to the store's `/graphql` endpoint.
pub(crate) const STORE_USER_AGENT: &str = "EpicGamesLauncher/14.0.8-22004686+++Portal+Release-Live";
/// The User-Agent of the launcher's anonymous manifest and chunk downloads from the CDN.
pub(crate) const CDN_USER_AGENT: &str =
    "EpicGamesLauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";
