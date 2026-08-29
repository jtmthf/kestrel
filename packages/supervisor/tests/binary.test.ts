// Tests the artifact `bun run build` ships, not a separately compiled copy.

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

test("the supervisor binary starts and exits with status 0", async () => {
  const supervisor = Bun.spawn({ cmd: [binary], stdout: "pipe", stderr: "pipe" });

  const status = await supervisor.exited;
  const stderr = await new Response(supervisor.stderr).text();

  expect(status, `the supervisor exited ${status}. stderr:\n${stderr}`).toBe(0);
  expect(stderr).toContain("supervisor started");
  expect(stderr).toContain("supervisor stopped");
});
