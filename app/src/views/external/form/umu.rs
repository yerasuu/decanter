use gpui::{Context, Div, FontWeight, div, prelude::*, px};
use rust_i18n::t;

use super::GameForm;
use crate::theme;
use crate::widgets::{ButtonKind, button};

impl GameForm {
    /// Looks the name up in the UMU database. An exact title match is taken
    /// straight away when nothing was picked yet; anything looser waits for
    /// the user to choose.
    pub(super) fn search_umu(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.error = t!("external.form.errors.name_for_umu").into_owned();
            cx.notify();
            return;
        }
        self.umu_searching = true;
        cx.notify();
        self._umu_work = Some(cx.spawn(async move |this, cx| {
            let found = crate::external::umu::search(&name).await;
            let _ = this.update(cx, |this, cx| {
                if this.umu.is_none()
                    && let Ok(matches) = &found
                    && let Some(exact) = matches.iter().find(|found| found.exact)
                {
                    this.umu = Some(exact.entry.clone());
                }
                this.umu_matches = Some(found);
                this.umu_searching = false;
                cx.notify();
            });
        }));
    }

    /// Which UMU entry the game launches as: automatic (`umu-default`, which
    /// some Proton builds map from the executable) or one the name found.
    pub(super) fn umu_section(&self, cx: &Context<Self>) -> Div {
        let chip = |id: gpui::ElementId, label: String, choice: Option<crate::external::UmuEntry>| {
            let selected = self.umu == choice;
            button(
                id,
                if selected {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Secondary
                },
                label,
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.umu = choice.clone();
                cx.notify();
            }))
        };
        let matches = match &self.umu_matches {
            Some(Ok(matches)) => matches.clone(),
            Some(Err(_)) | None => Vec::new(),
        };
        // A stored choice the latest search didn't turn up still shows, so
        // it can be seen and kept.
        let stored = self
            .umu
            .clone()
            .filter(|entry| !matches.iter().any(|found| &found.entry == entry));
        let status = match (&self.umu_matches, self.umu_searching) {
            (_, true) => Some(theme::muted(t!("external.form.umu.searching"))),
            (Some(Err(err)), false) => Some(theme::error(err.clone())),
            (Some(Ok(matches)), false) if matches.is_empty() => Some(theme::muted(t!("external.form.umu.not_found"))),
            (Some(Ok(_)) | None, false) => None,
        };
        div()
            .flex()
            .flex_col()
            .mt_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mb_2()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("external.form.umu.title")),
                    )
                    .child(
                        button(
                            "umu-search",
                            ButtonKind::Icon,
                            t!("external.form.umu.search"),
                            self.umu_searching,
                        )
                        .when(!self.umu_searching, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.search_umu(cx)))
                        }),
                    ),
            )
            .child(theme::muted(t!("external.form.umu.hint")).text_size(px(12.)).mb_2())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(chip(
                        "umu-auto".into(),
                        t!("external.form.umu.automatic").into_owned(),
                        None,
                    ))
                    .children(stored.map(|entry| {
                        let label = umu_label(None, &entry);
                        chip("umu-stored".into(), label, Some(entry))
                    }))
                    .children(matches.into_iter().enumerate().map(|(index, found)| {
                        let label = umu_label(Some(&found.title), &found.entry);
                        chip(("umu-match", index).into(), label, Some(found.entry))
                    })),
            )
            .children(status.map(|status| status.mt_2()))
    }
}

/// "Arknights: Endfield · umu-endfield", with the store when it isn't
/// umu's own.
fn umu_label(title: Option<&str>, entry: &crate::external::UmuEntry) -> String {
    let id = match &entry.store {
        Some(store) => format!("{} ({store})", entry.id),
        None => entry.id.clone(),
    };
    match title {
        Some(title) => format!("{title} · {id}"),
        None => id,
    }
}
