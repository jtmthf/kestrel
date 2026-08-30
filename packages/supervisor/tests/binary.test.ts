// Tests the artifact `bun run build` ships, not a separately compiled copy. What the
// supervisor does once it has a link to dial is proved against a real control plane by the
// primary test seam, in the kestrel crate.

import { beforeAll, expect, test } from "bun:test";
import { join } from "node:path";

const packageRoot = join(import.meta.dir, "..");
const binary = join(packageRoot, "dist", "kestrel-supervisor");

beforeAll(() => {
  const build = Bun.spawnSync({
    cmd: ["bun", "run", "build"],
    cwd: packageRoot,
    stdout: "pipe",
    stderr: "pipe",
  });

  if (build.exitCode !== 0) {
    throw new Error(`\`bun run build\` failed:\n${build.stderr.toString()}`);
  }
});

test("the supervisor binary starts and says it has no link to dial", async () => {
  const supervisor = Bun.spawn({
    cmd: [binary],
    env: { PATH: process.env["PATH"] ?? "" },
    stdout: "pipe",
    stderr: "pipe",
  });

  const status = await supervisor.exited;
  const stderr = await new Response(supervisor.stderr).text();

  expect(status, `the supervisor exited ${status}. stderr:\n${stderr}`).toBe(1);
  expect(stderr).toContain("supervisor started");
  expect(stderr).toContain("no link to dial");
});
