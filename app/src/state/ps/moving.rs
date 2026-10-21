//! Moving a dump to another of the profile's library folders, for a game
//! kept on a big slow disk or brought back to the fast one. Only the dump
//! moves: its data stays in the primary library's `psport` folder.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Context, Task};

use super::Ps;
use super::jobs::JobKey;
use crate::external::transfer::Progress;
use crate::i18n::Text;

impl Ps {
    /// Moves the dump in `dir` into `library_root`'s games folder. Refused
    /// while the game runs, converts or is starting, and it can't start
    /// until the move ends.
    pub fn move_dump(
        &mut self,
        dir: PathBuf,
        library_root: PathBuf,
        progress: Arc<Progress>,
        cx: &Context<Self>,
    ) -> Task<Result<(), Text>> {
        if self.launching.contains(&dir) || self.is_running(&JobKey::Game(dir.clone()), cx) {
            return Task::ready(Err(Text::new("storage.errors.running")));
        }
        self.moving.insert(dir.clone());
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking({
                let dir = dir.clone();
                move || crate::relocate::move_dump(&dir, &library_root, &progress)
            })
            .await
            .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                this.moving.remove(&dir);
                this.rescan(cx);
            });
            result
        })
    }
}
