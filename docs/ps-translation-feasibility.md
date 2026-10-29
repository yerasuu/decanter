# Translations for PS titles: feasibility of reusing the Atelier Ryza (A21) patch for other languages

Source examined: `/home/deck/data/Traducción A21` (read only) and the owner's Ryza dump
(`UP4108-CUSA16458_00-RYZA000000MASTER`, v01.09). Nothing was extracted or modified. kb: see the board task for this analysis.

## What the patch is

A PC (Steam) patch made of three executables and three PAK containers (388 MB):

| File | Size | What it looks like |
|---|---|---|
| `PACK00_04_01.pak` | 312 MB | 204 entries; the index is plain text and names `\data\x64\res_en\font\mainfont00_hd1080_0.g1t`: replacement **font and UI textures** of the EN slot |
| `PACK01.pak` | 14.5 MB | 8842 entries; index obfuscated. By size and count: text tables and dialogue |
| `PACK02.pak` | 46 MB | 2777 entries; index obfuscated |
| `Atelier_Ryza.exe`, `_Env.exe`, `_Launcher.exe` | 22/4/7 MB | game executables, the first dated after the others (patched or rebuilt to load the packs) |

All three PAKs share the header `00 00 02 00 | entry count | 0x10 | 0x0d`. The patch **replaces the English slot**: the
paths it names are `res_en`. It does not add a language.

## How the PS4 game stores text (read from the dump)

The PS4 build holds four languages side by side, JP, EN, SC and TC, as loose files, so a language is a set of folders:

| What | Path | Format | Size (EN) |
|---|---|---|---|
| String tables | `Saves/Text_<L>/*.xml` | plain XML, `<str String_No="20840449" Text="..."/>`, 139 files | 2.6 MB |
| System messages, recipes | `Saves/systemMessage`, `Saves/recipeIdea` | XML plus an encrypted twin `*.xml.e` (338 `.e` files in all, 1 in `Text_EN`) | 256 KB |
| Dialogue | `Event/event_<L>/<QN06…>/EVENT_MESSAGE_*.ebm` | binary `.ebm`, 1645 files | tens of MB |
| UI styles | `Saves/ui_<L>/` | XML, 242 files | 2.4 MB |
| UI textures and fonts | `Data/PS4/Res_<L>/{Font,ui,Ipu,Movie}` | `.g1t` (327 files) and `opening.mp4` | 1.5 GB; fonts `MainFont00/01_HD2160_0.g1t` are 33 MB each |
| Scripts | `Script/*.nut` | Squirrel, 55 files (not language specific) | 392 KB |

The strings are keyed by `String_No`, not by position, so a translation is a map from id to text and does not depend on
language. The eboot picks the language slot from the system language; the list is fixed at four, so a new language
cannot be a new folder: it has to **take the place of one slot** (EN is the natural one), the way the patch does on PC.

## Viability by piece

| Piece | Verdict | Why and what it needs |
|---|---|---|
| String tables (`Text_<L>`) | Easy | plain XML keyed by id; replace the EN files with the same ids. Check that the patch's ids match the PS4 v01.09 ids (the PC and PS4 versions may differ) |
| System messages, recipes (`.xml` and `.xml.e`) | Medium | the XML is plain, but the game may read the `.e` twin; need to know which one it loads and how `.e` is made (the header `00 00 00 02 00 01 de 29` looks like a size plus an encrypted blob) |
| Dialogue (`.ebm`) | Medium to hard | binary; the format has to be understood to rebuild it with new text, and line length limits apply. The PC `.ebm` may be reusable when the version matches |
| UI styles (`ui_<L>` XML) | Easy | plain XML; only text and layout tweaks |
| UI textures with baked-in text | Hard | `g1t` is platform specific: the PC patch ships x64 `g1t`, the PS4 needs the PS4 swizzle and format. Needs a G1T converter |
| Fonts | The deciding piece | `Res_EN/Font/*.g1t` are glyph atlases. Latin languages with accents (ES, PT, FR, DE, IT, CA) need the glyphs in the atlas; the patch's 312 MB pack suggests the EN fonts had to be replaced. Cyrillic, Greek, Korean, Arabic or Thai need new atlases and probably new width tables |
| Delivering it | Feasible | psport already mounts a mods overlay for games (the run log shows `Mods: …/mod-game-…`); the translation would be an overlay of the folders above over `/app0`, never a change to the dump |
| Opening the PAKs | The unknown | `PACK00_04_01` has a readable index; `PACK01/02` have an obfuscated one (repeating 16-byte patterns in the zero padding). The key or algorithm is most likely in the patched `Atelier_Ryza.exe` |

## Reuse for other languages

- **From the Spanish patch to another Latin language:** reuse the pipeline, not the text. The ids, the file set and the
  font work carry over. The translated strings do not: they have to come from a translation of the EN or JP tables for the
  new language (human, or machine translation with review). The Spanish text is a useful second reference for length and
  tone, nothing more.
- **Other scripts:** possible only with new font atlases and a check of how the engine measures glyphs. Expect the most work here.
- **Length limits:** dialogue boxes and UI labels are fixed size; translations longer than the English ones overflow. This is
  the main quality risk, whatever the language.

## Constraints

- The patch is a derivative of a commercial game's text: use it only on the owner's own dump, for personal use, and do not
  ship extracted text or fonts in the repository or in any release. decanter or psport would at most ship the **tooling**
  (extract, merge, overlay), never the content.
- psport rule: no game-specific hacks in the core. An overlay of replacement files over `/app0` is not one; a quirk would be.
- The PC and PS4 versions can drift (the PS4 dump is v01.09). Every id and every `.ebm` has to be checked against the
  PS4 files before use.

## Proposed phases

1. **Open the PAKs** (read only): recover the index format and key of `PACK01/02` (from the patched exe), list their
   entries, and measure the id overlap between the patched XML and the PS4 `Text_EN`. This answers most of the open questions at low cost.
2. **Text only overlay:** build the overlay for `Text_EN` and `ui_EN` with the Spanish strings, run Ryza headless and
   look for missing glyphs and overflows (frames at the title and the first menus).
3. **Dialogue:** understand `.ebm` and apply the same ids; limit to the lines that fit.
4. **Fonts and textures:** G1T conversion for PS4 and, if the Latin accents are missing, extended atlases.
5. **Another language:** repeat 2 to 4 with a new set of strings; the tooling already exists by then.
6. **Product shape:** decide (owner) whether this is a psport mod format, a decanter "language pack" feature in the PS
   section, or only a documented workflow.

## Open questions for the owner

- Which language is the patch (Spanish, by the folder name)? Which other languages do you want first?
- Is this for your own use only, or a feature decanter should offer other users (then only the tooling, with each user
  providing their own translation)?
- Is the PC patch the only source, or is there a translator team's text in another format (spreadsheet, PO)?
