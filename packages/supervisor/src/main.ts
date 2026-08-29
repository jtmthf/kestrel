#!/usr/bin/env bun

import { run } from "./supervisor.ts";

process.exit(await run());
