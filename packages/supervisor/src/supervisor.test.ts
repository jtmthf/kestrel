import { expect, test } from "bun:test";

import { type Diagnostics, run } from "./supervisor.ts";

function recorder(): Diagnostics & { lines: string[] } {
  const lines: string[] = [];
  return { lines, info: (message) => void lines.push(message) };
}

test("the supervisor says when it started", async () => {
  const diagnostics = recorder();

  await run(diagnostics, {});

  expect(diagnostics.lines).toContain("supervisor started");
});

test("the supervisor fails when it is given no link to dial", async () => {
  const diagnostics = recorder();

  await expect(run(diagnostics, {})).resolves.toBe(1);
  expect(diagnostics.lines.join("\n")).toContain("no link to dial");
});

test("the supervisor fails when it is given a link but no credential", async () => {
  const diagnostics = recorder();

  const status = await run(diagnostics, {
    KESTREL_LINK: "http://127.0.0.1:1",
    KESTREL_RUN: "01999cf2-0000-7000-8000-000000000000",
  });

  expect(status).toBe(1);
  expect(diagnostics.lines.join("\n")).toContain("no link to dial");
});
