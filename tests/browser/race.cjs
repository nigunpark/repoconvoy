const { chromium } = require('playwright');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');

const root = path.resolve(__dirname, '../..');
const projects = ['Alpha', 'Beta'].map((name, index) => ({
  id: String(index + 1), name,
  repositories: [{ name: `repo-${index + 1}`, path: `/fixture/repo-${index + 1}` }]
}));
const sessions = {
  '1': [{ id: 'alpha-1', projectId: '1', state: 'idle', children: [] },
        { id: 'alpha-2', projectId: '1', state: 'idle', children: [] }],
  '2': [{ id: 'beta-1', projectId: '2', state: 'idle', children: [] }]
};
let releaseAlpha;
const alphaGate = new Promise(resolve => { releaseAlpha = resolve; });
let signalAlpha;
const alphaIntercepted = new Promise(resolve => { signalAlpha = resolve; });
let signalAlphaDone;
const alphaDone = new Promise(resolve => { signalAlphaDone = resolve; });

(async () => {
  const options = { headless: true };
  if (process.env.BROWSER_EXECUTABLE) options.executablePath = process.env.BROWSER_EXECUTABLE;
  const browser = await chromium.launch(options);
  try {
    const page = await browser.newPage({ viewport: { width: 1365, height: 768 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.route('http://repoconvoy.test/**', async route => {
      const requestedPath = new URL(route.request().url()).pathname;
      if (requestedPath === '/') {
        return route.fulfill({ contentType: 'text/html', body: fs.readFileSync(path.join(root, 'web/dashboard.html')) });
      }
      const asset = {
        '/theme.css': ['web/theme.css', 'text/css'],
        '/theme.js': ['web/theme.js', 'text/javascript'],
        '/icons.svg': ['web/icons.svg', 'image/svg+xml'],
        '/assets/app-icon.svg': ['assets/app-icon.svg', 'image/svg+xml']
      }[requestedPath];
      if (asset) return route.fulfill({ contentType: asset[1], body: fs.readFileSync(path.join(root, asset[0])) });
      let data;
      if (requestedPath === '/api/projects') data = { projects };
      else if (requestedPath === '/api/capabilities') data = { dispatch: false };
      else if (requestedPath === '/api/projects/1/sessions') {
        signalAlpha();
        await alphaGate;
        await route.fulfill({ contentType: 'application/json', body: JSON.stringify({ sessions: sessions['1'] }) });
        signalAlphaDone();
        return;
      } else if (requestedPath === '/api/projects/2/sessions') data = { sessions: sessions['2'] };
      else if (requestedPath === '/api/sessions/beta-1/board') {
        data = { sessionId: 'beta-1', projectId: '2', state: 'idle', children: [] };
      } else throw new Error(`Unexpected API request: ${requestedPath}`);
      return route.fulfill({ contentType: 'application/json', body: JSON.stringify(data) });
    });
    await page.goto('http://repoconvoy.test/', { waitUntil: 'domcontentloaded' });
    await alphaIntercepted;
    await page.locator('#projects button').filter({ hasText: 'Beta' }).click();
    await page.locator('#session-count').getByText('1').waitFor();
    releaseAlpha();
    await alphaDone;
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await page.locator('#project-title').textContent(), 'Beta');
    assert.equal(await page.locator('#session-count').textContent(), '1', 'stale Alpha sessions under Beta');
    assert.equal(await page.locator('#sessions .session').count(), 1);
    await page.locator('#sessions .session').click();
    await page.locator('#board-scope').getByText(/beta-1/).waitFor();
    assert.deepEqual(errors, []);
    console.log('UI race test: Beta selection, session list and board remain scoped after stale Alpha response');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
