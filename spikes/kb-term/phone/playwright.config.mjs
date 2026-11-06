import { defineConfig, devices } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const proj = (name, device, browserName) => {
  const { defaultBrowserType, ...d } = devices[device];
  return { name, use: { ...d, browserName, hasTouch: true } };
};

export default defineConfig({
  testDir: HERE,
  testMatch: /.*\.spec\.mjs/,
  timeout: 180_000,
  workers: 1,
  fullyParallel: false,
  retries: 0,
  outputDir: path.join(HERE, 'out/pw-artifacts'),
  reporter: [['list'], ['json', { outputFile: path.join(HERE, 'out/pw.json') }]],
  use: { baseURL: 'http://127.0.0.1:4173' },
  webServer: {
    command: `node ${path.join(HERE, 'server.mjs')}`,
    url: 'http://127.0.0.1:4173/health',
    reuseExistingServer: false,
    timeout: 30_000,
  },
  projects: [
    proj('webkit-iphone15', 'iPhone 15', 'webkit'),
    proj('chromium-pixel7', 'Pixel 7', 'chromium'),
    proj('chromium-iphone15', 'iPhone 15', 'chromium'),
    proj('webkit-pixel7', 'Pixel 7', 'webkit'),
  ],
});
