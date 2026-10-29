// SPDX-License-Identifier: GPL-3.0-or-later
//! The async GOG client on the workspace's `reqwest`: one module per GOG host or area (see
//! `docs/own-gog-client-plan.md`).
mod achievements;
mod auth;
mod catalog;
mod content;
mod error;
mod galaxy_products;
mod gamesdb;
mod http;
mod orders;
mod products;
mod user;

pub use achievements::Achievement;
pub use auth::{CLIENT_ID, CLIENT_SECRET, REDIRECT_URI, Token};
pub use catalog::{CatalogMods, CatalogProduct};
pub use content::{Build, BuildMeta, DepotChunk, DepotFile, DepotMeta, SecureLinkEntry};
pub use error::Error;
pub use galaxy_products::{ContentSystems, Description, Dlc, Images, PRODUCT_BATCH, Product};
pub use gamesdb::{Company, GameDetails, Genre, Image, Localized, Release, parse_release_date};
pub use http::{Client, Sessionless};
pub use orders::{Order, OrdersPage};
pub use products::{ProductDetails, ProductsPage};
pub use user::User;
