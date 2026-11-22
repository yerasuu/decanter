//! *Identificar con Nexus*: the mods added from a file the user downloaded by hand, asked of Nexus
//! Mods by the name of the archive they came from, so they carry the mod's own name, credits and
//! update check. Asked only when the user presses it, since the acceptable use policy wants every
//! request to come from a user action.

use decanter_mods::ModId;
use decanter_nexus::{Error, GameSettings};
use gpui::Context;

use super::super::ModsTab;
use crate::i18n::Text;
use crate::nexus::failure_text;

#[derive(Default)]
struct Outcome {
    identified: usize,
    unmatched: usize,
    stopped: Option<Error>,
}

impl ModsTab {
    pub(in crate::views::library::detail::mods) fn unidentified(&self) -> Vec<(ModId, String)> {
        self.loaded
            .as_ref()
            .map(|loaded| {
                loaded
                    .rows
                    .iter()
                    .filter(|row| row.manifest.origin.is_none())
                    .map(|row| (row.manifest.id.clone(), row.manifest.source_file_name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(in crate::views::library::detail::mods) fn identify_with_nexus(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let pending = self.unidentified();
        if pending.is_empty() {
            return;
        }
        let domain = match GameSettings::read(&mods) {
            Ok(Some(settings)) => settings.domain,
            Ok(None) => {
                self.error = Some(Text::new("mods.nexus.identify_no_game"));
                cx.notify();
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "nexus game settings unreadable for identifying mods");
                self.error = Some(failure_text(&err));
                cx.notify();
                return;
            }
        };
        self.busy = Some(Text::new("mods.nexus.identifying"));
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = tokio::spawn(async move {
                let mut outcome = Outcome::default();
                let client = match crate::nexus::client().await {
                    Ok(client) => client,
                    Err(err) => {
                        outcome.stopped = Some(err);
                        return outcome;
                    }
                };
                for (id, source) in pending {
                    match decanter_nexus::identify(&client, &domain, &source).await {
                        Ok(Some(found)) => {
                            let origin = decanter_nexus::origin(&domain, found.info.id, &found.file, "")
                                .map(|origin| origin.with_credits(found.info.credits(&domain)));
                            let name = found.display_name();
                            let mods = mods.clone();
                            let written = match origin {
                                Ok(origin) => tokio::task::spawn_blocking(move || mods.identify(&id, origin, &name))
                                    .await
                                    .map_err(|err| err.to_string())
                                    .and_then(|result| result.map_err(|err| err.to_string())),
                                Err(err) => Err(err.to_string()),
                            };
                            match written {
                                Ok(()) => outcome.identified += 1,
                                Err(err) => {
                                    tracing::warn!(%err, source, "identified mod not recorded");
                                    outcome.unmatched += 1;
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::info!(source, "mod is no file of the nexus mods its name points at");
                            outcome.unmatched += 1;
                        }
                        Err(err) => {
                            tracing::warn!(%err, source, "identifying mods with nexus stopped");
                            outcome.stopped = Some(err);
                            break;
                        }
                    }
                }
                outcome
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(outcome) => {
                        this.notice = Some(
                            Text::new("mods.nexus.identified")
                                .arg("count", outcome.identified)
                                .arg("missing", outcome.unmatched),
                        );
                        this.error = outcome.stopped.as_ref().map(failure_text);
                    }
                    Err(err) => {
                        tracing::error!(%err, "identifying mods with nexus stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }
}
