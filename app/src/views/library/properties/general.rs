use std::path::PathBuf;
use std::rc::Rc;

use decanter_core::InstalledGame;
use gpui::{Context, Window, prelude::*};
use rust_i18n::t;

use super::{Target, inline, pane, row, rows};
use crate::views::library::{actions, data, fmt};
use crate::widgets::{ButtonKind, button};

pub(super) struct GeneralTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    seconds_played: i64,
    path: Option<PathBuf>,
}

impl GeneralTab {
    pub(super) fn new(target: Rc<Target>) -> Self {
        let account_id = target.account_id.as_str();
        let product_id = target.product_id.as_str();
        let installed = crate::core::installed_game(account_id, product_id);
        let seconds_played = data::load_playtime(account_id, target.storefront, product_id).seconds_total;
        let path = actions::game_install_path(account_id, product_id);
        Self {
            target,
            installed,
            seconds_played,
            path,
        }
    }
}

impl Render for GeneralTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let target = &self.target;
        let external = target.storefront == decanter_core::Storefront::Unspecified;
        let mut table = rows()
            .child(row(t!("library.properties.general.title"), target.title.clone()))
            .child(row(
                t!("library.filters.store.title"),
                if external {
                    t!("library.detail.external")
                } else {
                    "GOG".into()
                },
            ))
            .when(!external, |table| {
                table.child(row(
                    t!("library.properties.general.product_id"),
                    target.product_id.clone(),
                ))
            });
        if let Some(installed) = &self.installed {
            table = table
                .when(!external, |table| {
                    table.child(row(t!("library.properties.general.build"), installed.build_id.clone()))
                })
                .children(
                    installed
                        .size_on_disk
                        .map(|size| row(t!("library.properties.general.size"), crate::format::bytes(size))),
                );
        }
        table = table
            .child(row(
                t!("library.properties.general.played"),
                fmt::duration(self.seconds_played),
            ))
            .children(
                self.installed
                    .as_ref()
                    .and_then(|installed| installed.last_played)
                    .map(|last| {
                        row(
                            t!("library.detail.summary.last_session"),
                            fmt::unix_date(last).unwrap_or_default(),
                        )
                    }),
            );

        pane().child(table).children(self.path.clone().map(|path| {
            inline(
                button(
                    "open-folder",
                    ButtonKind::Secondary,
                    t!("library.properties.general.open_folder"),
                    false,
                )
                .on_click(move |_, _, _| {
                    if let Err(err) = crate::desktop::open(&path) {
                        tracing::error!(path = %path.display(), %err, "could not open the folder");
                    }
                }),
            )
        }))
    }
}
