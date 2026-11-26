// What happens to a Linear delivery the computer never took. It lives apart
// from linear.ts because the relay calls it, and linear.ts reaches the relay
// through machines.ts: importing it from the relay would make a cycle.

import type { Env } from "./util";

/// The relay dropped delivery `id` after a week unanswered. Task 7 tells the
/// Linear session here; until then there is nothing to tell.
export async function expired(_env: Env, _id: string): Promise<void> {}
