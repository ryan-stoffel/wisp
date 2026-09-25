// Builds one mock page from URL parameters:
//   ?scene=coordinator|agents|subagent|review|host|m0&theme=dark-modern|light-modern[&notes=0]
// The scenes are static pictures of wisp on upstream's Agents window, laid out after Ryan's
// Cursor Projects reference (#100). The teal and purple badges are design annotations;
// docs/design/agents-window.md explains each number.
'use strict';

const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'coordinator';
const theme = params.get('theme') ?? 'dark-modern';
const showNotes = params.get('notes') !== '0';

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;');

// Line icons drawn as inline SVG, in the spirit of codicons. No icon font, no emoji.
const ICON = {
	edit: '<path d="M3 13l1-3 7-7 2 2-7 7-3 1z"/>',
	search: '<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5"/>',
	clock: '<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3l2 1.5"/>',
	sliders: '<path d="M3 5h10M3 11h10"/><circle cx="6" cy="5" r="1.5"/><circle cx="10" cy="11" r="1.5"/>',
	plus: '<path d="M8 3v10M3 8h10"/>',
	filter: '<path d="M3 4h10l-4 5v3l-2 1V9z"/>',
	folderPlus: '<path d="M2 5h4l1 1.5h7V12H2z"/><path d="M9.5 9h3M11 7.5v3"/>',
	folder: '<path d="M2 4.5h4l1 1.5h7v6.5H2z"/>',
	gear: '<circle cx="8" cy="8" r="2"/><path d="M8 2v2M8 12v2M2 8h2M12 8h2M3.8 3.8l1.4 1.4M10.8 10.8l1.4 1.4M3.8 12.2l1.4-1.4M10.8 5.2l1.4-1.4"/>',
	chevron: '<path d="M6 4l4 4-4 4"/>',
	thumbUp: '<path d="M5 7v6H3V7zM5 7l2.5-4c1 0 1.5.7 1.3 1.6L8.4 6.5H12c.8 0 1.3.7 1.1 1.5l-1 4c-.2.6-.7 1-1.3 1H5"/>',
	thumbDown: '<path d="M5 9V3H3v6zM5 9l2.5 4c1 0 1.5-.7 1.3-1.6L8.4 9.5H12c.8 0 1.3-.7 1.1-1.5l-1-4c-.2-.6-.7-1-1.3-1H5"/>',
	copy: '<rect x="5.5" y="5.5" width="7.5" height="7.5" rx="1"/><path d="M3 10.5V3h7.5"/>',
	branch: '<circle cx="5" cy="4" r="1.5"/><circle cx="5" cy="12" r="1.5"/><circle cx="11" cy="6" r="1.5"/><path d="M5 5.5v5M11 7.5c0 2-6 1.5-6 3"/>',
	server: '<rect x="2.5" y="3" width="11" height="4" rx="1"/><rect x="2.5" y="9" width="11" height="4" rx="1"/><path d="M5 5h.01M5 11h.01"/>',
	monitor: '<rect x="2" y="3" width="12" height="8" rx="1"/><path d="M6 14h4M8 11v3"/>',
	close: '<path d="M4 4l8 8M12 4l-8 8"/>',
	more: '<path d="M4 8h.01M8 8h.01M12 8h.01"/>',
	arrowUp: '<path d="M8 13V3M4 7l4-4 4 4"/>',
	check: '<path d="M3 8.5l3 3 7-7"/>',
	circle: '<circle cx="8" cy="8" r="5"/>',
	sidebar: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M6 3v10"/>',
	expand: '<path d="M3 6V3h3M13 10v3h-3M3 3l4 4M13 13l-4-4"/>',
	doc: '<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>',
	code: '<path d="M6 4L2 8l4 4M10 4l4 4-4 4"/>',
};
const ic = (name, cls = '') => `<svg class="i ${cls}" viewBox="0 0 16 16">${ICON[name]}</svg>`;

// Sidebar: actions, Projects, Repositories, No Repo, and the footer (Ryan's reference).
function sidebar({ selected = '', empty = false, hostState = 'connected', hostOpen = false } = {}) {
	const projects = [
		['billing-migration', 'B', '', '2m'],
		['magic-link-auth', 'M', 'b', '1d'],
		['dependency-upkeep', 'D', 'c', '3d'],
	];
	const repos = [
		['wisp', [['Fix the flaky attach test', '4h'], ['Explain the LaunchAgent plist', '2d']]],
		['dotfiles', [['Audit macOS 27 defaults', '6d']]],
	];
	const noRepo = [['Compare Stripe and Paddle fees', '1w']];
	const host = hostState === 'none'
		? '<span class="dot offline"></span>Not connected'
		: '<span class="dot connected"></span>mac-mini';
	return `<div class="sidebar" id="sidebar">
		<div class="traffic"><span></span><span></span><span></span><span class="spacer"></span>${ic('sidebar')}</div>
		<div class="actions" id="actions">
			<div class="action${empty ? ' disabled' : ''}">${ic('edit')}New Chat<span class="kbd">&#8984;N</span></div>
			<div class="action">${ic('search')}Search<span class="kbd">&#8984;K</span></div>
			<div class="action" id="automations">${ic('clock')}Automations</div>
			<div class="action">${ic('sliders')}Customize</div>
		</div>
		<div class="lists" id="lists">
			<div class="sec" id="projects-sec">Projects<span class="tools">${ic('plus', 'sm')}</span></div>
			<div id="projects">${empty
				? '<div class="empty-note">No projects yet. A project runs on a host, so it shows here once wisp connects to one.</div>'
				: projects.map(([n, g, c, a]) => `<div class="item${selected === n ? ' selected' : ''}"><span class="proj-glyph ${c}">${g}</span><span class="name">${n}</span><span class="age">${a}</span></div>`).join('')}</div>
			${empty ? '' : `<div class="sec" id="repos-sec">Repositories<span class="tools">${ic('filter', 'sm')}${ic('folderPlus', 'sm')}</span></div>
			<div id="repos">${repos.map(([r, ts]) => `<div class="item repo">${ic('folder')}<span class="name">${r}</span></div>${ts.map(([t, a]) => `<div class="item thread${selected === t ? ' selected' : ''}"><span class="name">${t}</span><span class="age">${a}</span></div>`).join('')}`).join('')}</div>
			<div class="sec" id="norepo-sec">No Repo</div>
			<div>${noRepo.map(([t, a]) => `<div class="item"><span class="name">${t}</span><span class="age">${a}</span></div>`).join('')}</div>`}
		</div>
		<div class="foot" id="foot"><span class="avatar">RS</span><span class="uname">Ryan Stoffel</span><span class="grow"></span><span class="hostchip${hostOpen ? ' open' : ''}" id="hostchip">${host}</span><span class="iconbtn">${ic('gear')}</span></div>
	</div>`;
}

function tabbar(tabs) {
	return `<div class="tabbar" id="tabbar">${tabs.map((t) => `<span class="tab${t.active ? ' active' : ''}"${t.id ? ` id="${t.id}"` : ''}>${t.icon}${esc(t.label)}${t.closable ? `<span class="x">${ic('close', 'sm')}</span>` : ''}</span>`).join('')}
		<span class="iconbtn" id="tab-plus">${ic('plus')}</span><span class="grow"></span>
		<span class="idebtn" id="ide">${ic('code', 'sm')}IDE</span><span class="iconbtn">${ic('more')}</span></div>`;
}

const coordTab = (active) => ({ icon: '<span class="proj-glyph">B</span>', label: 'billing-migration', active });

function composer({ placeholder = 'Send follow-up', disabled = false, model = 'Claude Code · Max', branch = 'main', host = 'mac-mini' } = {}) {
	return `<div class="composer${disabled ? ' disabled' : ''}" id="composer">
		<span class="plus">${ic('plus', 'sm')}</span><span class="ph">${placeholder}</span>
		${model ? `<span class="model" id="model">${model} ${ic('chevron', 'sm')}</span>` : ''}
		<span class="send">${ic('arrowUp', 'sm')}</span></div>
		<div class="cfoot" id="cfoot"><span>${ic('branch', 'sm')}${branch}</span><span id="where">${host === 'Not connected' ? ic('server', 'sm') + 'Not connected' : ic('server', 'sm') + host}</span></div>`;
}

const feedback = `<div class="feedback">${ic('thumbUp')}${ic('thumbDown')}${ic('copy')}</div>`;
const worked = (t, id = '') => `<div class="worked"${id ? ` id="${id}"` : ''}>${ic('chevron', 'sm')}Worked ${t}</div>`;

function coordinatorTranscript() {
	return `<div class="transcript"><div class="col">
		<div class="bubble">Move billing from Recurly to Stripe. Invoices must look the same, and keep the ledger's history.</div>
		${worked('1m 10s', 'worked')}
		<div class="reply"><p>I read the billing code and saved what I found to shared context as <span class="link">research/stripe-invoices.md</span>. Here is the plan: four agents on mac-mini, each in its own worktree.</p>
			<div class="plan" id="plan"><div class="plan-head"><b>Plan, 4 tasks</b><span>Approved 10:42</span></div>
			<ol><li>Receive Stripe webhooks <span class="w">mac-mini</span></li><li>Render invoice PDFs from Stripe line items <span class="w">mac-mini</span></li><li>Fix currency rounding <span class="w">mac-mini</span></li><li>Backfill ledger entries <span class="w">mac-mini · after 1</span></li></ol></div></div>
		${feedback}
		<div class="bubble">Run it. Also check the invoice smoke test on this Mac.</div>
		${worked('14m 2s')}
		<div class="reply"><p><b>Render invoice PDFs</b> is ready for review: 3 files, tests passed. <b>Fix currency rounding</b> failed two tests in <code>rounding.spec.ts</code>; I think it needs banker's rounding. The others are still running.</p></div>
		${feedback}
	</div></div>`;
}

function dockCoordinator({ panel = false } = {}) {
	const rows = [
		['running', 'Receive Stripe webhooks', 'server', 'mac-mini', 'Writing tests'],
		['review', 'Render invoice PDFs', 'server', 'mac-mini', 'Needs review'],
		['failed', 'Fix currency rounding', 'server', 'mac-mini', 'Failed'],
		['running', 'Run the invoice smoke test', 'monitor', 'this Mac', 'Running'],
		['queued', 'Backfill ledger entries', 'server', 'mac-mini', 'Queued'],
	];
	const agentsPanel = `<div class="agents-panel" id="agents-panel">
		<div class="head">Agents<span class="grow"></span><span class="iconbtn">${ic('close')}</span></div>
		${rows.map(([s, t, i, w, st], k) => `<div class="agent-row${k === 1 ? ' hover' : ''}"${k === 1 ? ' id="agent-row"' : ''}><span class="dot ${s}"></span><span>${t}</span><span class="where">${ic(i, 'sm')}${w}</span><span class="state">${st}</span></div>`).join('')}
		<div class="agent-more">More</div></div>`;
	return `<div class="dock"><div class="dock-col">
		${panel ? agentsPanel : `<div class="actioncard" id="actioncard"><span class="dot review"></span><span><b>Render invoice PDFs</b> is ready for review. <span class="added">+214</span> <span class="removed">-38</span></span><span class="grow"></span><span class="btn secondary">Review</span><span class="btn">Accept</span></div>
		<div class="chips" id="chips"><span class="chip">Retry currency rounding with banker's rounding</span><span class="chip">Show the plan</span></div>`}
		<div class="chips"><span class="chip pill${panel ? ' open' : ''}" id="agents-pill">Agents <span class="dot running"></span><span class="muted">5</span></span></div>
		${composer()}
	</div></div>`;
}

function projectPanel({ tab = 'project', ctxOpen = false } = {}) {
	const tabs = `<div class="ptabs" id="ptabs"><span class="tab${tab === 'project' ? ' active' : ''}">Project</span><span class="tab${tab === 'changes' ? ' active' : ''}">Changes <span class="muted">3</span></span><span class="tab">Files</span><span class="iconbtn">${ic('plus', 'sm')}</span><span class="grow"></span><span class="iconbtn">${ic('expand', 'sm')}</span><span class="iconbtn">${ic('sidebar', 'sm')}</span></div>`;
	if (tab === 'changes') {
		return `<div class="card panel" id="panel">${tabs}<div class="pbody" id="changes">
			<div class="wt-head"><span class="dot review"></span>Render invoice PDFs<span class="grow"></span><span class="added">+214</span> <span class="removed">-38</span></div>
			<div class="wt-sub mono">wisp/invoice-pdf · worktree on mac-mini</div>
			<div class="file-row"><span class="m">M</span><span class="path">src/invoices/render.ts</span><span class="added">+120</span><span class="removed">-30</span></div>
			<div class="file-row"><span class="m">A</span><span class="path">src/invoices/stripeLineItems.ts</span><span class="added">+64</span></div>
			<div class="file-row"><span class="m">A</span><span class="path">src/invoices/creditNotes.ts</span><span class="added">+22</span></div>
			<div class="file-row"><span class="m">M</span><span class="path">test/invoices.spec.ts</span><span class="added">+30</span><span class="removed">-8</span></div>
			<div style="margin:10px 0 0 16px;display:flex;gap:6px"><span class="btn secondary">Open diff</span><span class="btn">Accept</span></div>
		</div></div>`;
	}
	const facts = [
		['check', 'billing repo', 'Main repo for this project: ~/src/billing on mac-mini'],
		['check', 'Plan approved', '4 tasks, 10:42'],
		['check', 'Shared context', '6 files, outside the repo'],
		['check', 'Coordinator account', 'Claude Code, Max'],
		['circle', 'Review', 'Render invoice PDFs is waiting'],
	];
	return `<div class="card panel" id="panel">${tabs}<div class="pbody">
		<div class="phead"><span class="proj-glyph">B</span>billing-migration</div>
		<div class="psub">Coordinator on mac-mini · started Tuesday</div>
		<div id="facts">${facts.map(([i, t, s]) => `<div class="fact"><span class="${i === 'check' ? 'ck' : 'open'}">${ic(i, 'sm')}</span><span>${t}<small>${s}</small></span></div>`).join('')}</div>
		<div class="psec" id="ctx-sec">Shared context<span class="grow"></span>${ic('plus', 'sm')}</div>
		<div id="ctx">
			<div class="ctx"${ctxOpen ? ' style="font-weight:600"' : ''}>${ic('doc', 'sm')}preferences.md<span class="who">you</span></div>
			<div class="ctx">${ic('doc', 'sm')}test-instructions.md<span class="who">invoice PDFs</span></div>
			<div class="ctx">${ic('doc', 'sm')}research/stripe-invoices.md<span class="who">coordinator</span></div>
			<div class="ctx">${ic('doc', 'sm')}recurly-rounding.md<span class="who">coordinator</span></div>
			<div class="ctx muted">2 more</div>
		</div>
	</div></div>`;
}

function sceneCoordinator({ panel = false } = {}) {
	return `<div class="window">${sidebar({ selected: 'billing-migration' })}
		<div class="main with-panel">
			<div class="card session" id="session">${tabbar([coordTab(true)])}${coordinatorTranscript()}${dockCoordinator({ panel })}</div>
			${projectPanel()}
		</div></div>`;
}

function sceneSubagent() {
	return `<div class="window">${sidebar({ selected: 'billing-migration' })}
		<div class="main with-panel">
			<div class="card session" id="session">${tabbar([coordTab(false), { id: 'sub-tab', icon: '<span class="dot review"></span>', label: 'Render invoice PDFs', active: true, closable: true }])}
				<div class="transcript" id="sub-transcript"><div class="col">
					<div class="bubble from"><span class="who">Task from the coordinator, 10:42</span>Render invoice PDFs from Stripe line items. The layout must match Recurly's exactly: see <span class="link">research/stripe-invoices.md</span>. Run <code>npm test -- invoices</code> before you finish.</div>
					${worked('11m 40s')}
					<div class="reply"><p>Invoices now render from Stripe line items through a small adapter, <code>stripeLineItems.ts</code>, so <code>render.ts</code> keeps its layout code. Tax lines match the three Recurly fixtures byte for byte. 48 tests pass.</p></div>
					${feedback}
					<div class="bubble" id="direct-msg">Also handle credit notes: they print as negative lines on the next invoice.</div>
					${worked('2m 3s')}
					<div class="reply"><p>Done. Credit notes now render as negative lines through <code>creditNotes.ts</code>, with a fixture from the Recurly export. I told the coordinator about the change.</p></div>
					${feedback}
				</div></div>
				<div class="dock"><div class="dock-col">${composer({ placeholder: 'Message this agent', branch: 'wisp/invoice-pdf' })}</div></div>
			</div>
			${projectPanel({ tab: 'changes' })}
		</div></div>`;
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
	]],
	['src/invoices/stripeLineItems.ts', '+64', [
		['hunk', '', '', '@@ -0,0 +1,64 @@'],
		['add', '', '1', "import type Stripe from 'stripe';"],
		['add', '', '2', ''],
		['add', '', '3', 'export interface LineItem {'],
		['add', '', '4', '  description: string;'],
		['add', '', '5', '  amount: number;'],
		['add', '', '6', '  currency: string;'],
	]],
];

function sceneReview() {
	const files = diffs.map(([p, stat, lines]) => `<div class="diff-file-head"><span class="mono">${p}</span><span class="grow"></span>${stat.split(' ').map((s) => `<span class="${s.startsWith('+') ? 'added' : 'removed'}">${s}</span>`).join(' ')}</div>
		${lines.map(([k, a, b, t]) => `<div class="diff-line ${k}"><span class="ln">${a}</span><span class="ln">${b}</span><span>${esc(t)}</span></div>`).join('')}`).join('');
	return `<div class="window">${sidebar({ selected: 'billing-migration' })}
		<div class="main with-detail">
			<div class="card session">${tabbar([coordTab(false), { icon: '<span class="dot review"></span>', label: 'Render invoice PDFs', active: true, closable: true }])}
				<div class="transcript"><div class="col">
					${worked('11m 40s')}
					<div class="reply"><p>Invoices now render from Stripe line items through a small adapter, so <code>render.ts</code> keeps its layout code. 48 tests pass.</p></div>${feedback}
				</div></div>
				<div class="dock"><div class="dock-col">${composer({ placeholder: 'Message this agent', branch: 'wisp/invoice-pdf' })}</div></div>
			</div>
			<div class="card editor" id="detail">
				<div class="ptabs"><span class="tab active">Review: Render invoice PDFs</span><span class="tab">Project</span><span class="tab">Changes</span><span class="grow"></span><span class="iconbtn">${ic('sidebar', 'sm')}</span></div>
				<div class="review-bar" id="review-bar"><span class="dot review"></span><b>Render invoice PDFs</b><span class="mono muted">wisp/invoice-pdf → main</span><span><span class="added">+236</span> <span class="removed">-38</span></span><span class="grow"></span><span class="btn secondary">Request changes</span><span class="btn">Accept</span></div>
				${files}
			</div>
		</div></div>`;
}

function sceneHost() {
	return `<div class="window">${sidebar({ selected: 'billing-migration', hostOpen: true })}
		<div class="quickpick" id="quickpick">
			<div class="qp-row focused"><span class="dot connected"></span><span>mac-mini</span><span class="muted">Connected</span><span class="d">wispd 0.4.0 over ssh · 4 agents running · up 6 days</span></div>
			<div class="qp-row"><span class="dot offline"></span><span>This Mac</span><span class="muted">Not running</span><span class="d">Start wispd on this Mac</span></div>
			<div class="qp-sep"></div>
			<div class="qp-row"><span></span><span>Add host...</span><span></span></div>
			<div class="qp-row"><span></span><span>Reconnect</span><span></span></div>
			<div class="qp-row"><span></span><span>Show wispd log</span><span></span></div>
		</div>
		<div class="main with-panel">
			<div class="card session">${tabbar([coordTab(true)])}${coordinatorTranscript()}${dockCoordinator()}</div>
			${projectPanel({ ctxOpen: true })}
		</div></div>`;
}

function sceneM0() {
	return `<div class="window">${sidebar({ empty: true, hostState: 'none' })}
		<div class="main solo">
			<div class="card session" id="session">
				<div class="empty-state" id="empty-state">
					<h2>No host connected</h2>
					<p>The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.</p>
					${composer({ placeholder: 'Not connected to a host', disabled: true, model: '', branch: '-', host: 'Not connected' })}
				</div>
			</div>
		</div></div>`;
}

// Annotation layout per scene: [number, label, kind, target selector, x, y, frame?]. x and y are
// functions of the target's bounding rectangle.
const NOTES = {
	coordinator: [
		[1, 'Actions · M1, Automations M6, Customize M2', 'later', '#actions', (r) => r.width - 60, () => 4],
		[2, 'Projects: one coordinator each · M1', 'later', '#projects', () => 80, () => -23],
		[3, 'Repositories: normal threads · M3', 'later', '#repos', () => 110, () => -22],
		[4, 'Host · M1', 'later', '#hostchip', () => -20, () => -26],
		[5, 'Tabs, IDE button · M1', 'later', '#tabbar', (r) => r.width - 190, (r) => r.height - 6],
		[6, 'Worked summaries · M4', 'later', '#worked', () => 140, () => -2],
		[7, 'Action card and chips · M4', 'later', '#actioncard', () => 10, () => -24],
		[8, 'Agents pill · M3', 'later', '#agents-pill', (r) => r.width + 8, () => 3],
		[9, 'Branch and host · M1', 'later', '#cfoot', () => 200, () => 0],
		[10, 'Project tab: facts, shared context · M1, M3', 'later', '#panel', () => 12, (r) => r.height - 32, true],
	],
	agents: [
		[8, 'Agents panel: status, title, where · M3', 'later', '#agents-panel', (r) => r.width - 270, () => -24, true],
		[11, 'Opens as a tab · M3', 'later', '#agent-row', () => 330, () => 5],
	],
	subagent: [
		[11, 'Subagent tab next to the coordinator · M3', 'later', '#sub-tab', () => 0, (r) => r.height + 8],
		[12, 'You can message it directly · M3', 'later', '#direct-msg', () => -230, () => 8],
		[13, "This worktree's changes · M3", 'later', '#changes', () => 0, (r) => r.height + 10, true],
	],
	review: [
		[14, 'Diff review in the detail pane · M3', 'later', '#detail', (r) => r.width - 260, (r) => r.height - 32, true],
		[15, 'Accept or request changes · M3', 'later', '#review-bar', (r) => r.width - 250, (r) => r.height + 6],
	],
	host: [
		[4, 'Host menu · M1', 'later', '#quickpick', () => 0, () => -26, true],
	],
	m0: [
		[16, 'wisp sidebar, empty · M0', 'm0', '#lists', () => 8, () => 130],
		[17, 'No-host custom view · M0', 'm0', '#empty-state', () => 12, () => 12, true],
		[4, 'Host: Not connected · M0 static', 'm0', '#hostchip', (r) => -110, () => -26],
		[18, 'Opens at launch · M0', 'm0', '#actions', () => 60, () => -30],
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
		Object.assign(b.style, { left: `${Math.min(1436 - 8 * label.length, Math.max(4, r.left + x(r)))}px`, top: `${Math.max(2, r.top + y(r))}px` });
		layer.appendChild(b);
	}
	const legend = document.createElement('div');
	legend.className = 'legend-strip';
	legend.innerHTML = '<span><i style="background:#00796b"></i>M0</span><span><i style="background:#5e35b1"></i>Later</span><span>Numbers: agents-window.md</span>';
	layer.appendChild(legend);
	document.body.appendChild(layer);
}

const scenes = {
	coordinator: () => sceneCoordinator(),
	agents: () => sceneCoordinator({ panel: true }),
	subagent: sceneSubagent,
	review: sceneReview,
	host: sceneHost,
	m0: sceneM0,
};
document.body.className = `theme-${theme}`;
document.getElementById('root').innerHTML = scenes[scene]();
if (showNotes) {
	placeNotes();
}
document.body.dataset.ready = 'true';
