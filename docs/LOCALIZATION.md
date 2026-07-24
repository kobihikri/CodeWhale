# Localization Matrix

Canonical tracking document for every locale Codewhale ships, is actively
building, is planning, or has explicitly deferred.

> **Scope note (2026-07-12):** this matrix tracks the website/README surface.
> The TUI ships its own locale packs under `crates/tui/locales/`
> (en, es-419, ja, ko, pt-BR, vi, zh-Hans complete; zh-Hant intentionally
> partial per #4057), guarded by raw key-parity tests in
> `crates/tui/src/localization.rs`. Keep the two surfaces distinct when
> updating status here.

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

## Website locales

| Locale | Code | Status | Notes |
|--------|------|--------|-------|
| English | `en` | **shipped** | Source text. Every page has an EN route. |
| Simplified Chinese | `zh` | **shipped** | Full parity with EN on all first-class pages. |
| Japanese | `ja` | **planned** | README exists (`README.ja-JP.md`); website route not yet live. Depends on locale-switcher supporting >2 languages and dictionary scaffolding (#3091). |
| Vietnamese | `vi` | **planned** | README exists (`README.vi.md`); same dependencies as Japanese (#3091). |
| Korean | `ko` | **planned** | README exists (`README.ko-KR.md`); #3093 next-wave locale. |
| Russian | `ru` | **planned** | **Next-priority locale.** No README yet; explicitly scoped for #3092. Cyrillic is **not** covered by the current webfonts — all four `next/font/google` families in `web/app/[locale]/layout.tsx` load `subsets: ["latin"]` only, so Cyrillic falls through to the `system-ui` fallback in `web/app/globals.css`. Needs a font-subset decision plus dictionary + route scaffolding. |
| Spanish | `es` | **deferred** | #3093 next-wave. |
| Brazilian Portuguese | `pt-BR` | **deferred** | #3093 next-wave. |
| Arabic | `ar` | **deferred** | RTL candidate. Deferred until layout/typography QA exists (bidirectional text, mirrored chrome, number formatting). |

## README locales

| Locale | File | Status | Parity check |
|--------|------|--------|-------------|
| English | `README.md` | **shipped** | Canonical source |
| Simplified Chinese | `README.zh-CN.md` | **shipped** | Manual review per release |
| Japanese | `README.ja-JP.md` | **shipped** | Manual review per release |
| Vietnamese | `README.vi.md` | **shipped** | Manual review per release |
| Korean | `README.ko-KR.md` | **shipped** | Manual review per release |
| Spanish (LatAm) | `README.es-419.md` | **shipped** | Machine-translated 2026-07-12; awaiting native review |
| Brazilian Portuguese | `README.pt-BR.md` | **shipped** | Machine-translated 2026-07-12; awaiting native review |
| Russian | _(not yet created)_ | **planned** | #3092 |

## Drift checks

| Check | Tool | Status |
|-------|------|--------|
| README locale links symmetric | `scripts/check-readme-locales.sh` | **Enforced** in CI (`.github/workflows/ci.yml`). Guards main-README ↔ file existence both ways; does *not* check cross-links between translations. |
| README translations in sync | `scripts/check-readme-translations.py` | **Enforced** in CI (`.github/workflows/ci.yml`) |
| Website dictionaries cover all shipped locales | `npm run check:locales` (vitest) | Planned |
| Accept-Language routes to all shipped locales | Middleware test | Planned |
| Locale selector lists all shipped locales | Component test | Planned |

## How to add a locale

1. Add the locale code to `locales` array in `web/lib/i18n/config.ts`.
2. Add label to `LOCALE_LABELS` in `web/components/locale-switcher.tsx`.
3. Scaffold translation dictionaries under `web/lib/i18n/dictionaries/<code>/`.
4. Add a locale route segment — Next.js `[locale]` will pick it up automatically once the `locales` array includes it.
5. Update this matrix.

## Related issues

- #3091 — Website parity with JA + VI README locales
- #3092 — Russian README + website localization
- #3093 — Korean, Spanish, Brazilian Portuguese next-wave locales
- #3087 — Post-rebrand README source text refresh
