# Localization Matrix

Canonical tracking document for every locale Codewhale ships, is actively
building, is planning, or has explicitly deferred.

> **Scope note (2026-07-24):** this matrix tracks three separate surfaces —
> the TUI locale packs, the README translations, and the website. A locale can
> ship on one and be deferred on another; the tables below are per-surface and
> must not be read as a single project-wide status.

Customer-visible copy also follows the [Codewhale voice and terminal
charter](VOICE.md); commands, key names, and glyphs remain code-owned around
localized prose.

Last updated: 2026-07-24.
Source-of-truth README: `README.md` (English, post-#3087).

## Status legend

| Status | Meaning |
|--------|---------|
| **shipped** | Live on codewhale.net and/or published as a standalone README |
| **partial** | Shipped but missing sections; actively being filled in |
| **planned** | Explicitly prioritized for the next wave |
| **deferred** | Acknowledged as wanted but not yet scheduled; needs layout QA, bridge support, or community champion |

---

## TUI locale packs

The TUI is the largest translation surface in the repo. The registry is
`Locale` in `crates/tui/src/localization.rs`; `Locale::shipped_complete()`
lists the packs held to `en.json` key parity and `Locale::is_partial_pack()`
lists the packs allowed to fall back to English. This table, those two
functions, and the pack files are gated together by
`python3 scripts/check-locale-matrix.py`.

| Locale | Pack | Keys vs `en.json` | Status |
|--------|------|-------------------|--------|
| English | `crates/tui/locales/en.json` | 1123 (source) | **shipped** |
| Japanese | `crates/tui/locales/ja.json` | 1123 / 1123 | **shipped** |
| Simplified Chinese | `crates/tui/locales/zh-Hans.json` | 1123 / 1123 | **shipped** |
| Brazilian Portuguese | `crates/tui/locales/pt-BR.json` | 1123 / 1123 | **shipped** |
| Latin American Spanish | `crates/tui/locales/es-419.json` | 1123 / 1123 | **shipped** |
| Vietnamese | `crates/tui/locales/vi.json` | 1123 / 1123 | **shipped** |
| Korean | `crates/tui/locales/ko.json` | 1123 / 1123 | **shipped** |
| Traditional Chinese | `crates/tui/locales/zh-Hant.json` | 478 / 1123 | **partial** |

`zh-Hant` is selectable at runtime and renders English for the 645 keys it
does not yet carry (#4057). It is excluded from `shipped_complete()` on
purpose; the gate still requires every key it *does* define to exist in
`en.json`, so a typo cannot hide behind the fallback.

## Website locales

| Locale | Code | Status | Notes |
|--------|------|--------|-------|
| English | `en` | **shipped** | Source text. Every page has an EN route. |
| Simplified Chinese | `zh` | **shipped** | Full parity with EN on all first-class pages. |
| Japanese | `ja` | **planned** | README exists (`README.ja-JP.md`); website route not yet live. Depends on locale-switcher supporting >2 languages and dictionary scaffolding (#3091). |
| Vietnamese | `vi` | **planned** | README exists (`README.vi.md`); same dependencies as Japanese (#3091). |
| Korean | `ko` | **planned** | Website only — README and a complete TUI pack already ship. #3093 next-wave locale. |
| Russian | `ru` | **planned** | **Next-priority locale.** No README yet; explicitly scoped for #3092. Cyrillic is **not** covered by the current webfonts — all four `next/font/google` families in `web/app/[locale]/layout.tsx` load `subsets: ["latin"]` only, so Cyrillic falls through to the `system-ui` fallback in `web/app/globals.css`. Needs a font-subset decision plus dictionary + route scaffolding. |
| Spanish | `es` | **deferred** | Website only — the TUI ships a complete `es-419` pack and `README.es-419.md` exists. #3093 next-wave for the site. |
| Brazilian Portuguese | `pt-BR` | **deferred** | Website only — the TUI ships a complete `pt-BR` pack and `README.pt-BR.md` exists. #3093 next-wave for the site. |
| Arabic | `ar` | **deferred** | RTL candidate. Deferred until layout/typography QA exists (bidirectional text, mirrored chrome, number formatting). |

## README locales

| Locale | File | Status | Parity check |
|--------|------|--------|-------------|
| English | `README.md` | **shipped** | Canonical source |
| Simplified Chinese | `README.zh-CN.md` | **shipped** | Manual review per release |
| Japanese | `README.ja-JP.md` | **shipped** | Manual review per release |
| Vietnamese | `README.vi.md` | **shipped** | Manual review per release |
| Korean | `README.ko-KR.md` | **shipped** | Manual review per release |
| Latin American Spanish | `README.es-419.md` | **shipped** | Manual review per release |
| Brazilian Portuguese | `README.pt-BR.md` | **shipped** | Manual review per release |
| Russian | _(not yet created)_ | **planned** | #3092 |

Link symmetry between `README.md` and these files is gated by
`scripts/check-readme-locales.sh`; this table itself is still reviewed by hand.

## Drift checks

| Check | Tool | Status |
|-------|------|--------|
| TUI packs match the registry and this matrix | `scripts/check-locale-matrix.py` | **Shipped** — runs in CI (`.github/workflows/ci.yml`) |
| TUI complete packs hold `en.json` key parity | `crates/tui/src/localization.rs` tests | **Shipped** |
| README locale links symmetric | `scripts/check-readme-locales.sh` | **Shipped** — runs in CI (`.github/workflows/ci.yml`) |
| README translations stay in sync | `scripts/check-readme-translations.py` | **Shipped** — runs in CI |
| Website dictionaries cover all shipped locales | `npm run check:locales` (vitest) | Planned — blocked on #3091; `web/lib/i18n/dictionaries/` does not exist yet |
| Accept-Language routes to all shipped locales | Middleware test | Planned — #3091 |
| Locale selector lists all shipped locales | Component test | Planned — #3091 |

## How to add a locale

Each surface is independent. Do the surfaces you are actually shipping, then
update this matrix — the CI gates read it.

### TUI pack

1. Add a `Locale` variant plus its `tag()`, `translation_target_name()`, and
   `include_str!` arm in `crates/tui/src/localization.rs`.
2. Add it to `Locale::shipped()`, and to `Locale::shipped_complete()` only when
   the pack has full `en.json` key parity. An in-progress pack goes in
   `Locale::is_partial_pack()` instead and falls back to English.
3. Create `crates/tui/locales/<tag>.json` following
   `crates/tui/locales/AGENTS.md`.
4. Add a row to the **TUI locale packs** table above with the real key count.
5. Run `python3 scripts/check-locale-matrix.py`.

### README

1. Create `README.<tag>.md` and link it from the `README.md` header.
2. Add a row to the **README locales** table above.
3. Run `bash scripts/check-readme-locales.sh` and
   `python3 scripts/check-readme-translations.py`.

### Website

1. Add the locale code to the `locales` array in `web/lib/i18n/config.ts`.
2. Add a label to `LOCALE_LABELS` in `web/components/locale-switcher.tsx`.
3. Scaffold translation dictionaries under `web/lib/i18n/dictionaries/<code>/`.
4. Add a locale route segment — Next.js `[locale]` will pick it up automatically once the `locales` array includes it.
5. Add a row to the **Website locales** table above.

## Related issues

- #3091 — Website parity with JA + VI README locales
- #3092 — Russian README + website localization
- #3093 — Korean, Spanish, Brazilian Portuguese next-wave locales
- #3087 — Post-rebrand README source text refresh
- #4057 — `zh-Hant` TUI pack completion
- #4787 — TUI table in this matrix + locale drift gates
