// Builds one mock page from URL parameters:
//   ?scene=coordinator|subagent|review|host|m0&theme=dark-modern|light-modern[&notes=0]
// The scenes are static pictures of wisp on upstream's Agents window. The pink, teal, and purple
// badges are design annotations; docs/design/agents-window.md explains each number.
'use strict';

const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'coordinator';
const theme = params.get('theme') ?? 'dark-modern';
const showNotes = params.get('notes') !== '0';

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;');

const STATUS = {
	running: 'Running',
	review: 'Needs review',
	queued: 'Queued',
	done: 'Done',
	failed: 'Failed',
	stopped: 'Stopped',
	idle: 'Idle',
};

// Sidebar data: projects are the Sessions list's workspace sections.
const projects = [
	{
		name: 'billing-migration', host: 'mac-mini',
		coordinator: { desc: '3 agents running, 1 to review', meta: '10:52' },
		agents: [
			{ id: 'stripe-webhooks', status: 'running', desc: 'Writing tests', meta: '14m' },
			{ id: 'invoice-pdf', status: 'review', desc: 'Needs review', meta: '<span class="added">+214</span> <span class="removed">-38</span>' },
			{ id: 'ledger-backfill', status: 'queued', desc: 'Queued: after stripe-webhooks', meta: '' },
			{ id: 'currency-rounding', status: 'failed', desc: 'Failed: 2 tests in rounding.spec.ts', meta: '' },
		],
	},
	{
		name: 'magic-link-auth', host: 'mac-mini',
		coordinator: { desc: 'Done: 2 agents accepted', meta: 'Tue' },
		agents: [],
	},
	{
		name: 'dependency-upkeep', host: 'this Mac',
		coordinator: { desc: 'Waiting for its Monday trigger', meta: 'Mon' },
		agents: [],
	},
];

function sessionList(selected) {
	return projects.map((p) => {
		const rows = [];
		rows.push(`<div class="row coordinator${selected === p.name + '/coordinator' ? ' selected' : ''}">
			<span class="icon"><span class="coord-glyph"></span></span>
			<span class="name">Coordinator</span><span class="meta">${p.coordinator.meta}</span>
			<span class="desc">${p.coordinator.desc}</span></div>`);
		for (const a of p.agents) {
			rows.push(`<div class="row child${selected === a.id ? ' selected' : ''}">
				<span class="icon"><span class="dot ${a.status}"></span></span>
				<span class="name">${a.id}</span><span class="meta">${a.meta}</span>
				<span class="desc">${a.desc}</span></div>`);
		}
		return `<div class="section-header"><span>${p.name}</span><span class="grow"></span><span class="host-tag">${p.host}</span></div>${rows.join('')}`;
	}).join('');
}

function sidebar(selected, { contextSelected = false } = {}) {
	return `<div class="sidebar" id="sidebar">
		<div class="sidebar-header"><span>Sessions</span><span class="grow"></span>
			<span class="new-button">New project <span class="kbd">&#8984;N</span></span></div>
		<div class="session-list" id="session-list">${sessionList(selected)}</div>
		<div class="sidebar-footer" id="shared-context">
			<div class="section-header" style="margin-top:0"><span>Shared context</span><span class="grow"></span><span class="host-tag">billing-migration</span></div>
			<div class="context-row${contextSelected ? ' selected' : ''}"><span class="doc-glyph"></span><span class="file">preferences.md</span><span class="who">you</span></div>
			<div class="context-row"><span class="doc-glyph"></span><span class="file">test-instructions.md</span><span class="who">stripe-webhooks</span></div>
			<div class="context-row"><span class="doc-glyph"></span><span class="file">research/stripe-invoices.md</span><span class="who">coordinator</span></div>
			<div class="context-row"><span class="doc-glyph"></span><span class="file muted">3 more</span></div>
		</div>
	</div>`;
}

function titlebar({ crumb, host = 'connected', open = false }) {
	const hostHtml = host === 'none'
		? '<span class="dot offline"></span><span>Not connected</span>'
		: '<span class="dot connected"></span><span>mac-mini</span><span class="muted">wispd 0.4</span>';
	return `<div class="titlebar">
		<div class="traffic"><span></span><span></span><span></span><span class="toggle"></span></div>
		<div class="command-center">${crumb}</div>
		<div class="title-right"><span class="host-pill${open ? ' is-open' : ''}" id="host-pill">${hostHtml}</span>
			<span class="layout-toggle"></span><span class="layout-toggle"></span></div>
	</div>`;
}

function composer({ placeholder = 'Message the coordinator', disabled = false, account = 'Claude Code: Max' } = {}) {
	return `<div class="composer${disabled ? ' disabled' : ''}" id="composer">
		<div class="placeholder">${placeholder}</div>
		<div class="composer-foot">
			${account ? `<span class="picker" id="account-picker">${account} &#9662;</span>` : ''}
			<span class="grow"></span><span class="send${disabled ? ' disabled' : ''}">&#8593;</span>
		</div></div>`;
}

function changesAux(focus) {
	return `<div class="card aux" id="aux">
		<div class="aux-tabs"><span class="active">Changes</span><span>Files</span><span>Terminal</span></div>
		<div class="aux-body">
			<div class="wt${focus === 'invoice-pdf' ? ' focus' : ''}" id="wt-invoice">
				<div class="wt-head"><span class="dot review"></span><span class="grow">invoice-pdf</span><span class="added">+214</span><span class="removed">-38</span></div>
				<div class="wt-sub mono">wisp/invoice-pdf · tests passed</div>
				<div class="file-row"><span class="m">M</span><span class="path">src/invoices/render.ts</span><span class="added">+120</span><span class="removed">-30</span></div>
				<div class="file-row"><span class="m">A</span><span class="path">src/invoices/stripeLineItems.ts</span><span class="added">+64</span></div>
				<div class="file-row"><span class="m">M</span><span class="path">test/invoices.spec.ts</span><span class="added">+30</span><span class="removed">-8</span></div>
				<span class="btn">Review changes</span>
			</div>
			${focus === 'invoice-pdf' ? '' : `<div class="wt">
				<div class="wt-head"><span class="dot running"></span><span class="grow">stripe-webhooks</span><span class="added">+88</span><span class="removed">-4</span></div>
				<div class="wt-sub mono">wisp/stripe-webhooks · running</div>
				<div class="file-row"><span class="m">A</span><span class="path">src/webhooks/stripe.ts</span><span class="added">+71</span></div>
				<div class="file-row"><span class="m">M</span><span class="path">src/server.ts</span><span class="added">+17</span><span class="removed">-4</span></div>
			</div>
			<div class="wt">
				<div class="wt-head"><span class="dot failed"></span><span class="grow">currency-rounding</span><span class="added">+12</span><span class="removed">-9</span></div>
				<div class="wt-sub mono">wisp/currency-rounding · 2 failed</div>
			</div>
			<div class="wt">
				<div class="wt-head"><span class="dot queued"></span><span class="grow">ledger-backfill</span><span class="muted">no changes</span></div>
			</div>`}
		</div></div>`;
}

function coordinatorTranscript() {
	return `<div class="transcript"><div class="transcript-inner">
		<div class="request">Move billing from Recurly to Stripe. Invoices must look the same, and keep the ledger's history.</div>
		<div class="response">
			<p>I read the billing code and wrote what I found to <span class="link">research/stripe-invoices.md</span>. Here is the plan: four agents on mac-mini, each in its own worktree.</p>
			<div class="plan" id="plan"><div class="plan-head"><b>Plan, 4 tasks</b><span>Approved 10:42</span></div>
				<ol>
					<li>Receive Stripe webhooks and verify signatures <span class="who">stripe-webhooks · mac-mini</span></li>
					<li>Render invoice PDFs from Stripe line items <span class="who">invoice-pdf · mac-mini</span></li>
					<li>Fix currency rounding to match Recurly <span class="who">currency-rounding · mac-mini</span></li>
					<li>Backfill ledger entries <span class="who">ledger-backfill · mac-mini · after 1</span></li>
				</ol></div>
		</div>
		<div class="event"><span>Started stripe-webhooks, invoice-pdf, currency-rounding</span><span class="line"></span><span>10:42</span></div>
		<div class="result" id="result"><span class="dot review"></span><span><b>invoice-pdf</b> finished: 3 files, <span class="added">+214</span> <span class="removed">-38</span>, tests passed</span><span class="grow"></span><span class="btn secondary">Open agent</span><span class="btn">Review changes</span></div>
		<div class="result error"><span class="dot failed"></span><span><b>currency-rounding</b> failed: 2 tests failed in <code>rounding.spec.ts</code></span><span class="grow"></span><span class="btn secondary">Open agent</span><span class="btn secondary">Retry</span></div>
		<div class="request">Retry currency-rounding with banker's rounding, and tell it about the Recurly export in shared context.</div>
		<div class="response"><p>Restarting currency-rounding with that instruction. I added <span class="link">recurly-rounding.md</span> to shared context so later agents see it too.</p></div>
	</div></div>`;
}

function sceneCoordinator() {
	return `<div class="window">
		${titlebar({ crumb: 'billing-migration <span class="muted">›</span> Coordinator' })}
		<div class="content classic">
			${sidebar('billing-migration/coordinator')}
			<div class="card session" id="session">
				<div class="session-header"><span class="coord-glyph"></span><span class="title">Coordinator</span><span class="sub">billing-migration · mac-mini · plans and delegates, never edits</span><span class="grow"></span><span class="status-label"><span class="dot running"></span>3 agents running</span></div>
				${coordinatorTranscript()}
				<div class="composer-wrap">${composer()}</div>
			</div>
			${changesAux()}
		</div>
	</div>`;
}

function sceneSubagent() {
	return `<div class="window">
		${titlebar({ crumb: 'billing-migration <span class="muted">›</span> invoice-pdf' })}
		<div class="content classic">
			${sidebar('invoice-pdf')}
			<div class="card session" id="session">
				<div class="session-header"><span class="dot review"></span><span class="title">invoice-pdf</span><span class="sub mono">wisp/invoice-pdf · mac-mini · Claude Code: Max · 12m</span><span class="grow"></span><span class="status-label">Needs review</span></div>
				<div class="transcript" id="agent-transcript"><div class="transcript-inner">
					<div class="request from-coordinator"><span class="from">Task from the coordinator, 10:42</span>Render invoice PDFs from Stripe line items. The layout must match Recurly's exactly: see <span class="link">research/stripe-invoices.md</span>. Run <code>npm test -- invoices</code> before you finish.</div>
					<div class="tool"><span class="check"></span>Read research/stripe-invoices.md, preferences.md</div>
					<div class="tool"><span class="check"></span>Searched for "RecurlyInvoice", 14 results</div>
					<div class="tool"><span class="check"></span>Edited src/invoices/render.ts</div>
					<div class="tool"><span class="check"></span>Created src/invoices/stripeLineItems.ts</div>
					<div class="tool"><span class="check"></span>Ran <code>npm test -- invoices</code>: 48 passed</div>
					<div class="response"><p>Invoices now render from Stripe line items through a small adapter, <code>stripeLineItems.ts</code>, so <code>render.ts</code> keeps its layout code. Tax lines and credits match the three Recurly fixtures byte for byte.</p><p>Wrote the fixture notes to shared context as <span class="link">test-instructions.md</span>.</p></div>
					<div class="result" id="result"><span class="dot review"></span><span>3 files, <span class="added">+214</span> <span class="removed">-38</span>, tests passed</span><span class="grow"></span><span class="btn">Review changes</span></div>
				</div></div>
				<div class="composer-wrap"><div class="readonly-bar" id="readonly"><span>Agents take their instructions from the coordinator.</span><span class="grow"></span><span class="btn secondary">Stop agent</span><span class="btn secondary">Ask the coordinator</span></div></div>
			</div>
			${changesAux('invoice-pdf')}
		</div>
	</div>`;
}

const diffs = [
	['src/invoices/render.ts', '+120 -30', [
		['hunk', '', '', '@@ -41,12 +41,18 @@ export function renderInvoice(invoice: Invoice)'],
		['', '41', '41', '  const doc = new PdfDocument(layout);'],
		['del', '42', '', '  for (const item of invoice.recurlyItems) {'],
		['del', '43', '', '    doc.row(item.description, formatMoney(item.unitAmountInCents));'],
		['add', '', '42', '  for (const item of toLineItems(invoice.stripe)) {'],
		['add', '', '43', '    doc.row(item.description, formatMoney(item.amount, item.currency));'],
		['add', '', '44', '    if (item.taxAmount) {'],
		['add', '', '45', '      doc.taxRow(item.taxRate, formatMoney(item.taxAmount, item.currency));'],
		['add', '', '46', '    }'],
		['', '44', '47', '  }'],
		['', '45', '48', '  doc.total(formatMoney(invoice.total, invoice.currency));'],
	]],
	['src/invoices/stripeLineItems.ts', '+64', [
		['hunk', '', '', '@@ -0,0 +1,64 @@'],
		['add', '', '1', "import type Stripe from 'stripe';"],
		['add', '', '2', ''],
		['add', '', '3', 'export interface LineItem {'],
		['add', '', '4', '  description: string;'],
		['add', '', '5', '  amount: number;'],
		['add', '', '6', '  currency: string;'],
		['add', '', '7', '  taxRate?: string;'],
	]],
];

function sceneReview() {
	const files = diffs.map(([path, stat, lines]) => `<div class="diff-file">
		<div class="diff-file-head"><span class="mono">${path}</span><span class="grow"></span><span>${stat.split(' ').map((s) => `<span class="${s.startsWith('+') ? 'added' : 'removed'}">${s}</span>`).join(' ')}</span></div>
		${lines.map(([k, a, b, t]) => `<div class="diff-line ${k}"><span class="ln">${a}</span><span class="ln">${b}</span><span>${esc(t)}</span></div>`).join('')}
	</div>`).join('');
	return `<div class="window">
		${titlebar({ crumb: 'billing-migration <span class="muted">›</span> invoice-pdf' })}
		<div class="content detail">
			${sidebar('invoice-pdf')}
			<div class="card session" id="session">
				<div class="session-header"><span class="dot review"></span><span class="title">invoice-pdf</span><span class="grow"></span><span class="status-label">Needs review</span></div>
				<div class="transcript"><div class="transcript-inner">
					<div class="tool"><span class="check"></span>Ran <code>npm test -- invoices</code>: 48 passed</div>
					<div class="response"><p>Invoices now render from Stripe line items through a small adapter, so <code>render.ts</code> keeps its layout code. Tax lines and credits match the three Recurly fixtures byte for byte.</p></div>
					<div class="result"><span class="dot review"></span><span>3 files, <span class="added">+214</span> <span class="removed">-38</span></span><span class="grow"></span><span class="btn secondary">Review changes</span></div>
				</div></div>
				<div class="composer-wrap"><div class="readonly-bar"><span>Agents take their instructions from the coordinator.</span><span class="grow"></span><span class="btn secondary">Ask the coordinator</span></div></div>
			</div>
			<div class="card editor" id="detail">
				<div class="editor-tabs"><span class="editor-tab active">Review: invoice-pdf</span><span class="editor-tab">Changes</span><span class="editor-tab">Files</span></div>
				<div class="review-bar" id="review-bar"><span class="dot review"></span><b>invoice-pdf</b><span class="mono muted">wisp/invoice-pdf → main</span><span><span class="added">+214</span> <span class="removed">-38</span></span><span class="muted">tests passed</span><span class="grow"></span><span class="btn secondary">Request changes</span><span class="btn">Accept</span></div>
				${files}
			</div>
		</div>
	</div>`;
}

function sceneHost() {
	return `<div class="window">
		${titlebar({ crumb: 'billing-migration <span class="muted">›</span> Shared context <span class="muted">›</span> preferences.md', open: true })}
		<div class="quickpick" id="quickpick">
			<div class="qp-row focused"><span class="dot connected"></span><span>mac-mini</span><span class="muted">Connected</span><span class="d">wispd 0.4.0 over ssh · 3 agents running · up 6 days</span></div>
			<div class="qp-row"><span class="dot offline"></span><span>This Mac</span><span class="muted">Not running</span><span class="d">Start wispd on this Mac</span></div>
			<div class="qp-sep"></div>
			<div class="qp-row"><span></span><span>Add host...</span><span></span></div>
			<div class="qp-row"><span></span><span>Reconnect</span><span></span></div>
			<div class="qp-row"><span></span><span>Show wispd log</span><span></span></div>
		</div>
		<div class="content detail">
			${sidebar('', { contextSelected: true })}
			<div class="card session">
				<div class="session-header"><span class="coord-glyph"></span><span class="title">Coordinator</span><span class="sub">billing-migration · mac-mini</span><span class="grow"></span><span class="status-label"><span class="dot running"></span>3 agents running</span></div>
				${coordinatorTranscript()}
				<div class="composer-wrap">${composer()}</div>
			</div>
			<div class="card editor" id="detail">
				<div class="editor-tabs"><span class="editor-tab active">preferences.md</span></div>
				<div class="context-bar" id="context-bar"><span class="doc-glyph"></span><span>Shared context on mac-mini, outside the repo. Every agent in billing-migration reads it.</span></div>
				<div class="md-editor"><span class="h"># Preferences</span>

- Use pnpm, not npm, for new scripts.
- Money is always integer minor units plus a currency code.
- Keep PDF layout code in <span class="h">src/invoices/render.ts</span>.
- Ask before adding a dependency over 1 MB.

<span class="h">## Testing</span>

- Invoice fixtures live in test/fixtures/recurly/.
- Run the invoice tests before finishing: npm test -- invoices
</div>
			</div>
		</div>
	</div>`;
}

function sceneM0() {
	return `<div class="window">
		${titlebar({ crumb: 'Wisp', host: 'none' })}
		<div class="content noaux">
			<div class="sidebar" id="sidebar">
				<div class="sidebar-header"><span>Sessions</span><span class="grow"></span><span class="new-button" style="opacity:.4">New project <span class="kbd">&#8984;N</span></span></div>
				<div class="empty-list" id="empty-list">No projects yet. Projects live on a host, so they show here once wisp connects to one.</div>
			</div>
			<div class="card session" id="session">
				<div class="empty-state" id="empty-state">
					<h2>No host connected</h2>
					<p>The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.</p>
					${composer({ placeholder: 'Not connected to a host', disabled: true, account: '' })}
				</div>
			</div>
		</div>
	</div>`;
}

// Annotation layout per scene: [number, label, kind, target selector, x, y, frame?]. x and y are
// functions of the target's bounding rectangle.
const NOTES = {
	coordinator: [
		[1, 'Projects · M1', 'later', '#session-list', () => 8, (r) => r.height - 34, true],
		[2, 'Coordinator · M4', 'later', '#session-list .row.coordinator', () => 124, () => 3],
		[3, 'Subagent · M3', 'later', '#session-list .row.child', () => 140, () => 3],
		[4, 'Host status · M1', 'later', '#host-pill', () => -40, (r) => r.height + 6],
		[5, 'Coordinator transcript · M4', 'later', '#plan', () => 150, () => 5],
		[6, 'Accounts · M2', 'later', '#account-picker', (r) => r.width + 8, () => 0],
		[7, 'Changes per worktree · M3', 'later', '#aux', () => 8, (r) => r.height - 32, true],
		[8, 'Shared context · M3', 'later', '#shared-context', (r) => r.width - 140, (r) => r.height - 28, true],
	],
	subagent: [
		[3, 'Status · M3', 'later', '#session-list .row.child.selected', () => 150, () => 3],
		[9, 'Read-only transcript · M3', 'later', '#agent-transcript', (r) => r.width - 230, () => 8],
		[10, 'No composer · M3', 'later', '#readonly', () => 8, () => -26],
		[7, "This worktree's changes · M3", 'later', '#wt-invoice', () => 8, (r) => r.height + 8, true],
	],
	review: [
		[11, 'Diff review in the detail pane · M3', 'later', '#detail', (r) => r.width - 260, (r) => r.height - 32, true],
		[12, 'Accept or request changes · M3', 'later', '#review-bar', (r) => r.width - 250, (r) => r.height + 6],
	],
	host: [
		[4, 'Host menu · M1', 'later', '#quickpick', () => 0, (r) => r.height + 8, true],
		[8, 'Shared context file · M3', 'later', '#context-bar', () => 8, () => 300],
	],
	m0: [
		[13, 'Copilot chrome removed · M0', 'm0', '#sidebar', () => 8, (r) => r.height - 34],
		[14, 'wisp provider, no sessions · M0', 'm0', '#empty-list', () => 8, (r) => r.height + 6],
		[15, 'No-host custom view · M0', 'm0', '#empty-state', () => 12, () => 12, true],
		[4, 'Static host status · M0', 'm0', '#host-pill', () => -130, (r) => r.height + 6],
	],
};

function placeNotes() {
	const layer = document.createElement('div');
	for (const [n, label, kind, sel, x, y, frame] of NOTES[scene] ?? []) {
		const el = document.querySelector(sel);
		if (!el) {
			throw new Error(`annotation target not found: ${sel}`);
		}
		const r = el.getBoundingClientRect();
		if (frame) {
			const f = document.createElement('div');
			f.className = `note-frame ${kind}`;
			Object.assign(f.style, { left: `${r.left - 2}px`, top: `${r.top - 2}px`, width: `${r.width + 4}px`, height: `${r.height + 4}px` });
			layer.appendChild(f);
		}
		const b = document.createElement('div');
		b.className = `note ${kind}`;
		b.innerHTML = `<span class="n">${n}</span>${esc(label)}`;
		Object.assign(b.style, { left: `${Math.max(4, r.left + x(r))}px`, top: `${Math.max(2, r.top + y(r))}px` });
		layer.appendChild(b);
	}
	const legend = document.createElement('div');
	legend.className = 'legend-strip';
	legend.innerHTML = '<span><i style="background:#00796b"></i>M0</span><span><i style="background:#5e35b1"></i>Later</span><span>Numbers: agents-window.md</span>';
	layer.appendChild(legend);
	document.body.appendChild(layer);
}

const scenes = { coordinator: sceneCoordinator, subagent: sceneSubagent, review: sceneReview, host: sceneHost, m0: sceneM0 };
document.body.className = `theme-${theme}`;
document.getElementById('root').innerHTML = scenes[scene]();
if (showNotes) {
	placeNotes();
}
document.body.dataset.ready = 'true';
