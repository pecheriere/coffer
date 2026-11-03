// The days the known answers are checked on: today, and the two ends of a month.
// No day can be in the future: a server's clock isn't ours to move, so a template
// seeded as of an earlier day stands in for that day, and a test asks about it by
// date. On the 1st, the first two are the same day.
import { addDays } from '../../seed/household.mjs';

export function days() {
  const today = process.env.STAGING_TODAY;
  const monthStart = `${today.slice(0, 8)}01`;
  const monthEnd = addDays(monthStart, -1);
  return { today, monthStart, monthEnd };
}
