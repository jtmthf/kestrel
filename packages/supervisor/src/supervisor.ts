/**
 * Holds no domain state. A branch here that reads domain state means the ADR-0001 split
 * has failed.
 */

export interface Diagnostics {
  info(message: string): void;
}

export const stderrDiagnostics: Diagnostics = {
  info(message) {
    process.stderr.write(`${message}\n`);
  },
};

export async function run(diagnostics: Diagnostics = stderrDiagnostics): Promise<number> {
  diagnostics.info("supervisor started");
  diagnostics.info("supervisor stopped");
  return 0;
}
