// Renders the mocks to ../<name>.png at 1440x900.
// Playwright is not a repo dependency. Run this from a temp directory outside the repo that
// has playwright-core installed; it is resolved from the current directory:
//   cd "$(mktemp -d)" && npm install playwright-core@1.63.0 && npx playwright-core install chromium
//   node <repo>/docs/design/coordinator-chat/src/render.mjs [name ...]
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const { chromium } = createRequire(join(process.cwd(), 'index.js'))('playwright-core');
const src = dirname(fileURLToPath(import.meta.url));
const out = dirname(src);

const pages = [
	['workbench-dark', 'page=workbench&theme=dark-modern&stage=m4'],
	['workbench-light', 'page=workbench&theme=light-modern&stage=m4'],
	['workbench-m0-dark', 'page=workbench&theme=dark-modern&stage=m0'],
	['state-1-empty', 'page=state&state=empty'],
	['state-2-conversation', 'page=state&state=conversation'],
	['state-3-running', 'page=state&state=running'],
	['state-4-error', 'page=state&state=error'],
	['option-b-agents-first', 'page=option-b&theme=dark-modern'],
];

const only = process.argv.slice(2);
const browser = await chromium.launch(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
try {
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
	const page = await context.newPage();
	page.on('pageerror', (error) => {
		throw error;
	});
	for (const [name, query] of pages) {
		if (only.length && !only.includes(name)) {
			continue;
		}
		await page.goto(`${pathToFileURL(join(src, 'index.html')).href}?${query}`);
		await page.waitForSelector('body[data-ready="true"]');
		await page.evaluate(() => document.fonts.ready);
		await page.screenshot({ path: join(out, `${name}.png`) });
		console.log(`${name}.png`);
	}
} finally {
	await browser.close();
}
