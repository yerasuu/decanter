// SPDX-License-Identifier: GPL-3.0-or-later
//! Who decanter says it is to Amazon, in one place so that the strings can change together.
//!
//! Amazon publishes no API and no third-party client policy for its games service, so every
//! value here is the official Amazon Games launcher's as nile sends it
//! ([nile](https://github.com/imLinguin/nile), `nile/constants.py` and `nile/api/authorization.py`
//! as read in docs/amazon-nile-integration-analysis.md, sections 1.2 and 2.1; the
//! [community research wiki](https://github.com/Lariaa/GameLauncherResearch/wiki/Amazon-Games)
//! nile cites). UNVERIFIED against Amazon: the owner's spike with stock nile (kb #1519) confirms
//! or corrects them. Registering as that launcher is the account risk the owner accepted (decision
//! #1518, D2).

/// Amazon's auth service: device registration, token refresh and deregistration.
pub(crate) const API_HOST: &str = "api.amazon.com";
/// The sign-in page lives on the store's own host, not on [`API_HOST`].
pub(crate) const SIGN_IN_URL: &str = "https://amazon.com/ap/signin";
/// Where the sign-in page sends the browser once the user logs in; the code rides in its query.
pub(crate) const RETURN_TO: &str = "https://www.amazon.com";
/// The hosts a pasted redirect may come from, since [`RETURN_TO`] can land on either spelling.
pub(crate) const RETURN_HOSTS: [&str; 2] = ["www.amazon.com", "amazon.com"];
/// The query parameter the redirect carries the authorization code in.
pub(crate) const CODE_PARAM: &str = "openid.oa2.authorization_code";

/// The launcher's device type: the sign-in page and the registration both name it, and the
/// client id is derived from it.
pub(crate) const DEVICE_TYPE: &str = "A2UMVHOX7UP4V7";
/// The sign-in page's association handle and page id for the launcher.
pub(crate) const ASSOC_HANDLE: &str = "amzn_sonic_games_launcher";
/// amazon.com, the only marketplace decanter supports (decision #1518, D11).
pub(crate) const MARKETPLACE_ID: &str = "ATVPDKIKX0DER";

/// The registration and refresh both name the app; the values must stay the launcher's.
pub(crate) const APP_NAME: &str = "AGSLauncher for Windows";
pub(crate) const APP_VERSION: &str = "1.0.0";
pub(crate) const DEVICE_MODEL: &str = "Windows";
pub(crate) const OS_VERSION: &str = "10.0.19044.0";
/// The User-Agent of the launcher's calls to [`API_HOST`].
pub(crate) const USER_AGENT: &str = "AGSLauncher/1.0.0";
