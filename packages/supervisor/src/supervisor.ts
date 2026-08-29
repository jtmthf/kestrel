/**
 * The supervisor: the process that runs inside an Environment alongside the agent runtime.
 *
 * It is TypeScript because of the Claude Agent SDK's `canUseTool` round-trip, and it holds
 * no domain state of its own — it forwards, it holds a cursor, and it heartbeats. If a
 * branch ever appears here that reads domain state, the split in ADR-0001 has failed.
 */

/** Where the supervisor writes diagnostics. Injected so a test can read them back. */
export interface Diagnostics {
  info(message: string): void;
}

/** stderr, so stdout stays free for anything the supervisor is asked to hand back. */
export const stderrDiagnostics: Diagnostics = {
  info(message) {
    process.stderr.write(`${message}\n`);
  },
};

/**
 * Runs the supervisor, resolving to the status the process should exit with.
 *
 * At 0.1 it starts and stops. The outward link to the control plane arrives in 0.1/04, and
 * the agent-runtime driver it hosts in 0.1/06.
 */
export async function run(diagnostics: Diagnostics = stderrDiagnostics): Promise<number> {
  diagnostics.info("supervisor started");
  diagnostics.info("supervisor stopped");
  return 0;
}
