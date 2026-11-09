//! The DLL overrides a mod asks Wine for (design, section 4): suggested at import for a proxy DLL,
//! editable per mod, and shown against the other enabled mods that ask for the same DLL, where the
//! later one in the load order wins.

use decanter_mods::{DllMode, ModId};
use gpui::{Context, Div, ElementId, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::{ModsTab, Row};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::Select;

/// What a mode means, next to its `WINEDLLOVERRIDES` spelling.
fn mode_label(mode: DllMode) -> SharedString {
    let meaning = match mode {
        DllMode::NativeThenBuiltin => t!("mods.dll.native_then_builtin"),
        DllMode::BuiltinThenNative => t!("mods.dll.builtin_then_native"),
        DllMode::Native => t!("mods.dll.native"),
        DllMode::Builtin => t!("mods.dll.builtin"),
        DllMode::Disabled => t!("mods.dll.disabled"),
    };
    if mode == DllMode::Disabled {
        meaning.into_owned().into()
    } else {
        format!("{} ({meaning})", mode.as_str()).into()
    }
}

impl ModsTab {
    pub(super) fn dll_block(&self, index: usize, row: &Row, locked: bool, cx: &Context<Self>) -> Option<Div> {
        if row.manifest.dll_overrides.is_empty() {
            return None;
        }
        let loaded = self.loaded.as_ref()?;
        let name_of = |id: &ModId| loaded.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        let lines = row.manifest.dll_overrides.iter().enumerate().map(|(position, wanted)| {
            let options = DllMode::ALL
                .into_iter()
                .map(|mode| (SharedString::from(mode.as_str()), mode_label(mode)));
            let id = row.manifest.id.clone();
            let dll = wanted.dll.clone();
            let select_id = ElementId::Name(format!("mod-dll-{index}-{position}").into());
            let select = Select::new(select_id, options, wanted.mode.as_str()).when(!locked, |select| {
                let entity = cx.entity();
                select.on_change(move |chosen, _, cx| {
                    let Ok(mode) = chosen.parse::<DllMode>() else {
                        return;
                    };
                    let (id, dll) = (id.clone(), dll.clone());
                    entity.update(cx, |this, cx| this.set_dll_mode(id, dll, mode, cx));
                })
            });
            let standing = row
                .enabled
                .then(|| loaded.dll.iter().find(|effective| effective.dll == wanted.dll))
                .flatten()
                .and_then(|effective| {
                    if effective.from != row.manifest.id {
                        Some(t!(
                            "mods.dll.loses",
                            name = name_of(&effective.from),
                            mode = effective.mode.as_str()
                        ))
                    } else if effective.over.is_empty() {
                        None
                    } else {
                        let over = effective
                            .over
                            .iter()
                            .map(|(other, _)| name_of(other))
                            .collect::<Vec<_>>()
                            .join(", ");
                        Some(t!("mods.dll.wins", names = over))
                    }
                });
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(theme::mono().w(px(110.)).child(wanted.dll.clone()))
                .child(select)
                .children(standing.map(theme::muted))
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(29.))
                .text_size(px(12.5))
                .child(theme::muted(t!("mods.dll.title")))
                .children(lines),
        )
    }

    fn set_dll_mode(&mut self, id: ModId, dll: String, mode: DllMode, cx: &mut Context<Self>) {
        self.run(
            Text::new("library.properties.mods.applying"),
            move |mods, _| mods.set_dll_mode(&id, &dll, mode),
            cx,
        );
    }
}
