// ADR-0001's split, as something you can fail a build on: the supervisor forwards, blocks,
// relays, holds a cursor and heartbeats, and never reasons about the domain.

import { expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const domain = [
  "session",
  "workspace",
  "organization",
  "transcript",
  "campaign",
  "trigger",
  "workflow",
  "approval",
  "policy",
  "audit",
];

function sources(directory: string): string[] {
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    return statSync(path).isDirectory() ? sources(path) : [path];
  });
}

test("nothing in the supervisor names a thing only the control plane may reason about", () => {
  for (const source of sources(join(import.meta.dir, "..", "src"))) {
    const spoken = readFileSync(source, "utf8").toLowerCase();

    for (const word of domain) {
      expect(spoken, `${source} names ${word}, which is the control plane's to know`).not.toContain(
        word,
      );
    }
  }
});
