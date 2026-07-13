import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

export async function runAwaitedPollLoop(
  cycle: () => Promise<void>,
  wait: () => Promise<void>,
  shouldContinue: () => boolean = () => true
): Promise<void> {
  while (shouldContinue()) {
    await cycle();
    if (shouldContinue()) await wait();
  }
}

async function selfTest() {
  let active = 0;
  let maximumActive = 0;
  let completed = 0;
  await runAwaitedPollLoop(
    async () => {
      active += 1;
      maximumActive = Math.max(maximumActive, active);
      await Promise.resolve();
      active -= 1;
      completed += 1;
    },
    async () => undefined,
    () => completed < 3
  );
  assert.equal(maximumActive, 1, "poll_cycles_never_overlap");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await selfTest();
