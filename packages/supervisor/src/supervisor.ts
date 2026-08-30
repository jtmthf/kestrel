/**
 * Holds no domain state. A branch here that reads domain state means the ADR-0001 split
 * has failed.
 */

import manifest from "../package.json" with { type: "json" };
import { Link, Refused } from "./link/client.ts";

const RECONNECT_AFTER = 250;

export interface Diagnostics {
  info(message: string): void;
}

export const stderrDiagnostics: Diagnostics = {
  info(message) {
    process.stderr.write(`${message}\n`);
  },
};

export async function run(
  diagnostics: Diagnostics = stderrDiagnostics,
  variables: Record<string, string | undefined> = process.env,
): Promise<number> {
  diagnostics.info("supervisor started");

  const link = dialled(variables);
  if (link === undefined) {
    diagnostics.info(
      "no link to dial: set KESTREL_LINK, KESTREL_RUN and KESTREL_RUN_CREDENTIAL",
    );
    return 1;
  }

  let cursor: string | undefined;

  for (;;) {
    try {
      const instructions = await link.open(cursor);
      diagnostics.info(cursor === undefined ? "link open" : `link open after ${cursor}`);
      await link.report({ kind: "connected", version: manifest.version });
      diagnostics.info("reported connected");

      for await (const delivered of instructions) {
        cursor = delivered.id;
        diagnostics.info(`instruction ${delivered.instruction.kind} ${delivered.id}`);

        if (delivered.instruction.kind === "stop") {
          diagnostics.info("supervisor stopped");
          return 0;
        }
      }

      diagnostics.info("lost the link");
    } catch (error) {
      if (error instanceof Refused) {
        diagnostics.info(`the link refused this environment: ${error.message}`);
        return 1;
      }
      diagnostics.info(`lost the link: ${error instanceof Error ? error.message : error}`);
    }

    await Bun.sleep(RECONNECT_AFTER);
  }
}

function dialled(variables: Record<string, string | undefined>): Link | undefined {
  const base = variables["KESTREL_LINK"];
  const run = variables["KESTREL_RUN"];
  const credential = variables["KESTREL_RUN_CREDENTIAL"];

  return base && run && credential ? new Link(base, run, credential) : undefined;
}
