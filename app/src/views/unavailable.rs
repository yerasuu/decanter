//! What the main window shows when the active library's disk isn't there:
//! decanter never creates a library folder it was pointed at, since an
//! unplugged drive's mount point would end up holding an empty library.

use std::path::{Path, PathBuf};

use gpui::{Context, Window, div, prelude::*, px, relative};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub struct Unavailable {
    root: PathBuf,
    /// Known libraries whose disk is there, to switch to.
    others: Vec<PathBuf>,
    error: Option<Text>,
}

impl Unavailable {
    pub fn new(root: PathBuf) -> Self {
        let others = decanter_core::bootstrap::known_roots()
            .into_iter()
            .filter(|known| *known != root && known.is_dir())
            .collect();
        Self {
            root,
            others,
            error: None,
        }
    }

    fn switch(&mut self, root: &Path, cx: &mut Context<Self>) {
        if let Err(err) = decanter_core::bootstrap::activate(root) {
            tracing::error!(root = %root.display(), %err, "cannot switch library");
            self.error = Some(Text::new("messages.errors.save_libraries").arg("error", err));
            cx.notify();
            return;
        }
        let session = AppState::get(cx).session.clone();
        session.update(cx, |session, cx| {
            session.reload();
            cx.notify();
        });
    }

    fn pick(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("chrome.unavailable.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            let picked = folder.path();
            // A library root itself, or a folder to make one under.
            let root = if decanter_core::bootstrap::db_path(picked).is_file() {
                picked.to_path_buf()
            } else {
                decanter_core::Library::root_under(picked)
            };
            let created = std::fs::create_dir_all(&root);
            let _ = this.update(cx, |this, cx| match created {
                Ok(()) => this.switch(&root, cx),
                Err(err) => {
                    this.error = Some(
                        Text::new("messages.errors.create_dir")
                            .arg("path", root.display())
                            .arg("error", err),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for Unavailable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().child(
            div()
                .w(px(480.))
                .max_w(relative(0.8))
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .gap_3()
                .child(theme::h1(t!("chrome.unavailable.title")))
                .child(theme::muted(t!("chrome.unavailable.body", path = self.root.display())))
                .children(self.others.iter().enumerate().map(|(index, root)| {
                    let root = root.clone();
                    button(
                        ("library-switch", index),
                        ButtonKind::Secondary,
                        t!("chrome.unavailable.use", path = root.display()),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.switch(&root, cx)))
                }))
                .child(
                    button(
                        "library-pick",
                        ButtonKind::Primary,
                        t!("chrome.unavailable.pick"),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.pick(cx))),
                )
                .children(self.error.as_ref().map(theme::error)),
        )
    }
}
