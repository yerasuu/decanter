//! Adding a dump from anywhere into the primary games folder (decision
//! #1107): the folder is checked once where it comes in, the user picks
//! copy or move every time, and the copy can be cancelled without leaving
//! half a dump behind.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use decanter_ps::library::{self, Game};
use gpui::{AppContext as _, Context};

use super::Ps;
use crate::external::transfer::{Mode, Progress};
use crate::i18n::Text;

/// A dump that passed the checks, waiting for copy or move.
pub struct Offer {
    pub source: PathBuf,
    pub game: Game,
    pub target: PathBuf,
    pub size: u64,
    /// Free bytes on the games folder's disk, when it can be read.
    pub free: Option<u64>,
}

pub enum Adding {
    Asking(Offer),
    Running {
        offer: Offer,
        mode: Mode,
        progress: Arc<Progress>,
    },
}

impl Ps {
    pub const fn adding(&self) -> Option<&Adding> {
        self.adding.as_ref()
    }

    /// What the latest add, or a dump put in its own folder, came to:
    /// `(text, is_error)`.
    pub const fn add_notice(&self) -> Option<&(Text, bool)> {
        self.add_notice.as_ref()
    }

    pub fn dismiss_add_notice(&mut self, cx: &mut Context<Self>) {
        self.add_notice = None;
        cx.notify();
    }

    /// Asks for a folder, then offers it like a dropped one.
    pub fn pick_dump(&mut self, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(rust_i18n::t!("ps.add.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.offer_dump(folder.path().to_path_buf(), None, cx));
        })
        .detach();
    }

    /// Checks `source` and asks copy or move. `game` is what
    /// [`library::inspect`] already read from it, for a drop that had to
    /// tell a dump from anything else first.
    pub fn offer_dump(&mut self, source: PathBuf, game: Option<Game>, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            self.add_notice = Some((Text::new("ps.add.refused.busy"), true));
            cx.notify();
            return;
        }
        let root = decanter_ps::paths::active();
        let games = root.games();
        let mut folders: Vec<PathBuf> = self
            .library_roots
            .iter()
            .map(|library_root| decanter_ps::paths::games_in(library_root))
            .collect();
        folders.push(games.clone());
        let check = cx.background_spawn(async move {
            let game = match game {
                Some(game) => game,
                None => crate::achv_try!(library::inspect(&source).map_err(|refusal| {
                    tracing::info!(path = %source.display(), %refusal, "folder refused as a PS dump");
                    crate::relocate::dump_refusal(&source, &refusal)
                })),
            };
            let target = crate::achv_try!(crate::relocate::dump_target(&source, &game, &games, &folders));
            let size = decanter_core::directory_size(&source)
                .inspect_err(|err| tracing::warn!(path = %source.display(), %err, "could not measure a PS dump"))
                .unwrap_or(0);
            let free = decanter_core::disk_usage(root.library()).map(|(free, _)| free);
            Ok::<_, Text>(Offer {
                source,
                game,
                target,
                size,
                free,
            })
        });
        cx.spawn(async move |this, cx| {
            let offer = check.await;
            let _ = this.update(cx, |this, cx| {
                match offer {
                    Ok(offer) if this.adding.is_none() => {
                        tracing::info!(source = %offer.source.display(), title_id = offer.game.title_id, "PS dump offered");
                        this.add_notice = None;
                        this.adding = Some(Adding::Asking(offer));
                    }
                    Ok(_) => this.add_notice = Some((Text::new("ps.add.refused.busy"), true)),
                    Err(err) => this.add_notice = Some((err, true)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Copies or moves the offered dump into place.
    pub fn start_add(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let Some(Adding::Asking(offer)) = self.adding.take() else {
            return;
        };
        let progress = Arc::new(Progress::default());
        let (source, target) = (offer.source.clone(), offer.target.clone());
        let work = Arc::clone(&progress);
        self.adding = Some(Adding::Running { offer, mode, progress });
        cx.notify();
        // The copy runs on a blocking thread that can't notify anyone, so
        // the progress is redrawn on a timer until it ends.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let still = this.update(cx, |this, cx| {
                    cx.notify();
                    this.adding.is_some()
                });
                if !matches!(still, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(move || crate::relocate::add_dump(&source, &target, mode, &work))
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                let Some(Adding::Running { offer, .. }) = this.adding.take() else {
                    return;
                };
                let title = if offer.game.title.is_empty() {
                    &offer.game.title_id
                } else {
                    &offer.game.title
                };
                this.add_notice = Some(match result {
                    Ok(()) => (
                        Text::new("ps.add.done")
                            .arg("title", title)
                            .arg("path", offer.target.display()),
                        false,
                    ),
                    Err(err) => (Text::new("ps.add.failed").arg("title", title).text("error", err), true),
                });
                this.rescan(cx);
            });
        })
        .detach();
    }

    /// Drops the offer, or stops the copy under way; a rename already
    /// under way finishes.
    pub fn cancel_add(&mut self, cx: &mut Context<Self>) {
        match &self.adding {
            Some(Adding::Asking(_)) => self.adding = None,
            Some(Adding::Running { progress, .. }) => progress.cancel(),
            None => {}
        }
        cx.notify();
    }
}
