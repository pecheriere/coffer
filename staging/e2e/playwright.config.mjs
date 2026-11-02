import { defineConfig } from '@playwright/test';

import { days } from './support/days.mjs';

// Set once here for the workers too, so a run crossing midnight stays on one day.
process.env.STAGING_TODAY ??= new Date().toISOString().slice(0, 10);
days();

export default defineConfig({
  globalSetup: './support/global-setup.mjs',
  outputDir: './test-results',
  forbidOnly: true,
  fullyParallel: true,
  workers: process.env.CI ? 2 : 4,
  // The longest test waits a minute for the rates job.
  timeout: 120_000,
  reporter: [['list']],
  use: { screenshot: 'only-on-failure' },
  projects: [
    { name: 'api', testDir: './api' },
    { name: 'ui', testDir: './ui' },
  ],
});
