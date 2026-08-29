import { expect, test } from "bun:test";

import { type Diagnostics, run } from "./supervisor.ts";

function recorder(): Diagnostics & { lines: string[] } {
  const lines: string[] = [];
  return { lines, info: (message) => void lines.push(message) };
}

test("the supervisor reports success when it has nothing left to do", async () => {
  const diagnostics = recorder();

  await expect(run(diagnostics)).resolves.toBe(0);
});

test("the supervisor says when it started and when it stopped", async () => {
  const diagnostics = recorder();

  await run(diagnostics);

  expect(diagnostics.lines).toContain("supervisor started");
  expect(diagnostics.lines).toContain("supervisor stopped");
});
