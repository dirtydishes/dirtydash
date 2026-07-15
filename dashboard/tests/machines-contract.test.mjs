import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("../src/machines.tsx", import.meta.url), "utf8");
const styles = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");

// Browser-oriented contract checks: these are intentionally close to the DOM
// vocabulary a Playwright/a11y pass will assert when a browser runner is
// available in CI.
assert.match(source, /aria-current=\{selected\.state === state \? "step"/);
assert.match(source, /role="status" aria-live="polite"/);
assert.match(source, /role="alert"/);
assert.match(source, /role="dialog" aria-modal="true"/);
assert.match(source, /aria-expanded=\{archiveOpen\}/);
assert.match(source, /read-only at this width/);
assert.match(source, /isMachineAdminWidth\(width: number\)/);
assert.match(source, /DELETE \{machine\.display_name\}/);
assert.match(source, /deleteConfirmation !== `DELETE \$\{machine\.display_name\}`/);
assert.match(source, /snapshot Hub/);
assert.match(source, /independent rollback/);
assert.match(source, /signature_verified/);
assert.match(source, /protocol current/);
assert.match(styles, /:focus-visible\s*\{/);
assert.match(styles, /@media \(max-width: 759px\)/);
assert.match(styles, /prefers-reduced-motion: reduce/);

console.log("Machines DOM/a11y contract: passed");
