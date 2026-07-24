import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  DOC_TOPICS,
  docTopicIsExternal,
  getGuideTopicsByCategory,
  getReferenceTopics,
} from "./docs-map";

const DOCS_SEARCH = readFileSync(
  new URL("../components/docs-search.tsx", import.meta.url),
  "utf8",
);
const DOCS_SIDEBAR = readFileSync(
  new URL("../components/docs-sidebar.tsx", import.meta.url),
  "utf8",
);

describe("docs index does not present repository structure as product docs", () => {
  it("never renders repo source paths in the docs index or sidebar", () => {
    // The index is documentation, not a file listing. Source paths stay in
    // docs-map.ts for the check:docs parity gate; they are not reader-facing.
    expect(DOCS_SEARCH).not.toContain("repoSource");
    expect(DOCS_SIDEBAR).not.toContain("repoSource");
    expect(DOCS_SEARCH).not.toContain("docs-topic-source");
  });

  it("gives unwritten topics their own section instead of mixing them into categories", () => {
    expect(DOCS_SEARCH).toContain("REFERENCE_HEADING");
    expect(DOCS_SEARCH).toContain('id="reference-docs"');
  });

  it("lists every unwritten topic exactly once, apart from the guides", () => {
    const reference = getReferenceTopics();
    const guides = [...getGuideTopicsByCategory().values()].flat();

    expect(reference.every(docTopicIsExternal)).toBe(true);
    expect(guides.length + reference.length).toBe(DOC_TOPICS.length);
    expect(new Set([...guides, ...reference]).size).toBe(DOC_TOPICS.length);
  });

  it("still renders at least one written guide", () => {
    expect([...getGuideTopicsByCategory().values()].flat().length).toBeGreaterThan(0);
  });
});
