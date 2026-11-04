use std::path::PathBuf;
use std::rc::Rc;

use decanter_core::{InstalledGame, Storefront};
use decanter_gog::{GalaxyStatus, PlayTask, galaxy_status};
use gpui::{Context, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, inline, pane, row, rows};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::{actions, data, fmt};
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button};

pub(super) struct GeneralTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    seconds_played: i64,
    path: Option<PathBuf>,
    /// The programs the game's `goggame-<id>.info` lists, primary first.
    tasks: Vec<PlayTask>,
    galaxy: Option<GalaxyStatus>,
    error: Option<Text>,
}

impl GeneralTab {
    pub(super) fn new(target: Rc<Target>) -> Self {
        let account_id = target.account_id.as_str();
        let product_id = target.product_id.as_str();
        let installed = crate::core::installed_game(account_id, product_id);
        let seconds_played = data::load_playtime(account_id, target.storefront, product_id).seconds_total;
        let path = actions::game_install_path(account_id, product_id);
        let tasks = match (&installed, &path) {
            (Some(game), Some(path)) if game.storefront == Storefront::Gog => {
                decanter_gog::play_tasks(path, product_id)
            }
            _ => Vec::new(),
        };
        let galaxy = match (&installed, &path) {
            (Some(game), Some(path)) if game.storefront == Storefront::Gog => crate::core::db()
                .inspect_err(|err| tracing::warn!(error = %err, "failed to open db, no Galaxy status to show"))
                .ok()
                .map(|db| galaxy_status(&db, path, product_id)),
            _ => None,
        };
        Self {
            target,
            installed,
            seconds_played,
            path,
            tasks,
            galaxy,
            error: None,
        }
    }

    fn choose(&mut self, executable: &str, cx: &mut Context<Self>) {
        match set_executable(&self.target.account_id, &self.target.product_id, executable) {
            Ok(()) => {
                self.error = None;
                self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id);
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    /// Which of the game's programs PLAY starts, offered only when the info
    /// file lists more than one.
    fn executable_picker(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.tasks.len() < 2 {
            return None;
        }
        let current = self.installed.as_ref()?.launch_executable.clone().unwrap_or_default();
        let mut options: Vec<(SharedString, SharedString)> = self
            .tasks
            .iter()
            .map(|task| {
                let label = if task.is_primary {
                    t!("library.properties.general.primary", name = task.name).into_owned()
                } else {
                    task.name.clone()
                };
                (SharedString::from(task.path.clone()), SharedString::from(label))
            })
            .collect();
        // A program picked before (by hand, or by the guess when the info
        // file named no primary) stays shown rather than reading as another.
        if !current.is_empty() && !self.tasks.iter().any(|task| task.path == current) {
            options.push((SharedString::from(current.clone()), SharedString::from(current.clone())));
        }
        let this = cx.entity().downgrade();
        let select = Select::new("game-executable", options, current).on_change(move |value, _, cx| {
            let _ = this.update(cx, |this, cx| this.choose(value, cx));
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mb_4()
                .child(theme::muted(t!("library.properties.general.executable")))
                .child(field_row().child(select)),
        )
    }
}

/// Records `executable` (one of the game's play tasks, relative to its
/// install directory) as what PLAY starts.
fn set_executable(account_id: &str, product_id: &str, executable: &str) -> Result<(), Text> {
    let library = crate::achv_try!(crate::core::library_for_account(account_id).inspect_err(
        |err| tracing::error!(account_id, product_id, error = %err, "failed to resolve library, launch executable not changed")
    ));
    // Re-read rather than editing the copy this tab rendered from, so a
    // change made elsewhere since isn't clobbered.
    let mut current = crate::achv_try!(
        crate::core::installed_game(account_id, product_id).ok_or_else(|| Text::new("messages.errors.not_installed"))
    );
    current.launch_executable = Some(executable.to_owned());
    match library.write_manifest(&current) {
        Ok(()) => {
            tracing::info!(account_id, product_id, executable, "launch executable changed");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, product_id, executable, error = %err, "failed to save launch executable");
            Err(Text::new("messages.errors.save_manifest").arg("error", err))
        }
    }
}

impl Render for GeneralTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        table = table.children(self.galaxy.map(|status| {
            row(
                t!("library.properties.general.galaxy"),
                match status {
                    GalaxyStatus::Active => t!("library.properties.general.galaxy_active"),
                    GalaxyStatus::PortHeld => t!("library.properties.general.galaxy_port_held"),
                    GalaxyStatus::NotAvailable => t!("library.properties.general.galaxy_not_available"),
                    GalaxyStatus::TurnedOff => t!("library.properties.general.galaxy_turned_off"),
                },
            )
        }));

        pane()
            .child(table)
            .children(self.executable_picker(cx))
            .children(error_line(self.error.as_ref()))
            .children(self.path.clone().map(|path| {
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
