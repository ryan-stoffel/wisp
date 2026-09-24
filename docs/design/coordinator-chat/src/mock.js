// Builds each mock from URL parameters. See render.mjs for the list of pages.
//   ?page=workbench&theme=dark-modern&stage=m4
//   ?page=state&state=empty|conversation|running|error
//   ?page=option-b&theme=dark-modern

const params = new URLSearchParams(location.search);
const page = params.get('page') ?? 'workbench';

// Plain shapes where VS Code draws codicons. No icon fonts.
const svg = (w, h, body) => `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
const shape = {
	files: () => svg(24, 24, '<path d="M8 4h7l4 4v11H8z"/><path d="M5 7v14h11"/>'),
	search: () => svg(24, 24, '<circle cx="10.5" cy="10.5" r="5.5"/><path d="M14.5 14.5l5 5"/>'),
	scm: () => svg(24, 24, '<circle cx="7" cy="6" r="2"/><circle cx="7" cy="18" r="2"/><circle cx="17" cy="9" r="2"/><path d="M7 8v8M17 11c0 3-4 3-8.5 5"/>'),
	extensions: () => svg(24, 24, '<rect x="4" y="11" width="6" height="6"/><rect x="10" y="11" width="6" height="6"/><rect x="4" y="17" width="6" height="3" rx="0"/><rect x="13" y="4" width="6" height="6"/>'),
	gear: () => svg(24, 24, '<circle cx="12" cy="12" r="3"/><circle cx="12" cy="12" r="7.5" stroke-dasharray="3 2.4"/>'),
	projects: () => svg(24, 24, '<rect x="4" y="5" width="16" height="4" rx="1"/><rect x="4" y="11" width="16" height="4" rx="1"/><rect x="4" y="17" width="10" height="3" rx="1"/>'),
	more: () => svg(16, 16, '<circle cx="3.5" cy="8" r="0.9" fill="currentColor"/><circle cx="8" cy="8" r="0.9" fill="currentColor"/><circle cx="12.5" cy="8" r="0.9" fill="currentColor"/>'),
	close: () => svg(16, 16, '<path d="M4 4l8 8M12 4l-8 8"/>'),
	maximize: () => svg(16, 16, '<rect x="3" y="3" width="10" height="10" rx="1"/>'),
	plus: () => svg(16, 16, '<path d="M8 3v10M3 8h10"/>'),
	split: () => svg(16, 16, '<rect x="2.5" y="3" width="11" height="10" rx="1"/><path d="M8 3v10"/>'),
	chevronDown: () => svg(16, 16, '<path d="M4.5 6.5L8 10l3.5-3.5"/>'),
	chevronRight: () => svg(16, 16, '<path d="M6.5 4.5L10 8l-3.5 3.5"/>'),
	arrowLeft: () => svg(16, 16, '<path d="M12 8H4M7 5L4 8l3 3"/>'),
	arrowRight: () => svg(16, 16, '<path d="M4 8h8M9 5l3 3-3 3"/>'),
	layoutCustom: () => svg(16, 16, '<rect x="2.5" y="3" width="11" height="10" rx="1"/><path d="M6 3v10M6 8h7.5"/>'),
	layoutLeft: () => svg(16, 16, '<rect x="2.5" y="3" width="11" height="10" rx="1"/><rect x="2.5" y="3" width="4" height="10" fill="currentColor" stroke="none"/>'),
	layoutPanel: () => svg(16, 16, '<rect x="2.5" y="3" width="11" height="10" rx="1"/><rect x="2.5" y="9" width="11" height="4" fill="currentColor" stroke="none"/>'),
	layoutRight: () => svg(16, 16, '<rect x="2.5" y="3" width="11" height="10" rx="1"/><rect x="9.5" y="3" width="4" height="10" fill="currentColor" stroke="none"/>'),
	errorCount: () => svg(14, 14, '<circle cx="7" cy="7" r="5"/><path d="M5 5l4 4M9 5l-4 4"/>'),
	warningCount: () => svg(14, 14, '<path d="M7 2.2L12.4 11.5H1.6z"/><path d="M7 6v2.4"/>'),
	branch: () => svg(14, 14, '<circle cx="4" cy="3.5" r="1.4"/><circle cx="4" cy="10.5" r="1.4"/><circle cx="10" cy="5" r="1.4"/><path d="M4 5v4M10 6.4c0 2-3 2.2-5.4 3"/>'),
};

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

// ---------------------------------------------------------------- chat view

function contextBar({ project, host, hostState, hover }) {
	const projectHtml = project
		? `<span class="wisp-chat-context-button${hover === 'project' ? ' is-hover' : ''}"><span class="wisp-chat-project">${esc(project)}</span><span class="wisp-chat-caret"></span></span>`
		: '<span class="wisp-chat-context-button"><span class="wisp-chat-project is-none">No project</span></span>';
	let hostHtml;
	if (!host) {
		hostHtml = '<span class="wisp-chat-context-button wisp-chat-host is-none"><span class="wisp-chat-dot is-none"></span>Not connected</span>';
	} else if (hostState === 'offline') {
		hostHtml = `<span class="wisp-chat-context-button wisp-chat-host"><span class="wisp-chat-dot is-offline"></span>${esc(host)}<span class="wisp-chat-host-state is-offline">offline</span></span>`;
	} else {
		hostHtml = `<span class="wisp-chat-context-button wisp-chat-host"><span class="wisp-chat-dot is-connected"></span>${esc(host)}</span>`;
	}
	return `<div class="wisp-chat-context">${projectHtml}${hostHtml}</div>`;
}

function message(from, time, body) {
	const author = from === 'user' ? 'You' : 'Coordinator';
	return `<div class="wisp-chat-message from-${from}"><div class="wisp-chat-author">${author}<span class="wisp-chat-time">${time}</span></div><div class="wisp-chat-body">${body}</div></div>`;
}

function composer({ placeholder, disabled, focused, stop, account, draft }) {
	const cls = ['wisp-chat-composer', disabled ? 'is-disabled' : '', focused ? 'is-focused' : ''].join(' ');
	const text = draft
		? `${esc(draft)}${focused ? '<span class="wisp-chat-caret-bar"></span>' : ''}`
		: `${focused ? '<span class="wisp-chat-caret-bar"></span>' : ''}<span class="placeholder">${esc(placeholder)}</span>`;
	const button = stop
		? '<span class="wisp-chat-button is-secondary">Stop</span>'
		: `<span class="wisp-chat-button${disabled || !draft ? ' is-disabled' : ''}">Send</span>`;
	const accountHtml = account ? `<span class="wisp-chat-account">${esc(account)}<span class="wisp-chat-caret"></span></span>` : '<span></span>';
	return `<div class="${cls}"><div class="wisp-chat-input">${text}</div><div class="wisp-chat-composer-footer">${accountHtml}${button}</div></div>`;
}

function agentsStrip({ open, summary, rows = [] }) {
	const header = `<div class="wisp-chat-agents-header"><span class="wisp-chat-chevron${open ? ' is-open' : ''}"></span><strong>Agents</strong><span class="wisp-chat-meta">${summary}</span></div>`;
	const body = open ? rows.map((r) => `<div class="wisp-chat-agent-row${r.hover ? ' is-hover' : ''}"><span class="wisp-chat-dot is-${r.state}"></span><span class="wisp-chat-agent-name">${r.name}</span><span class="wisp-chat-agent-task">${r.task}</span><span class="wisp-chat-agent-state">${r.status}</span></div>`).join('') : '';
	return `<div class="wisp-chat-agents">${header}${body}</div>`;
}

const planTasks = (states) => [
	['Magic-link endpoint and token store', 'magic-link-api'],
	['Email form and link landing page', 'login-ui'],
	['Password flow behind <code>auth.passwordLogin</code>', 'password-flag'],
].map(([title, agent], i) => {
	const [state, label] = states[i];
	return `<div class="wisp-chat-task"><span class="wisp-chat-task-number">${i + 1}</span><div><div>${title}</div><div class="wisp-chat-meta wisp-chat-task-status"><span class="wisp-chat-dot is-${state}"></span>${agent} · ${label}</div></div></div>`;
}).join('');

function planCard(states, header, actions) {
	return `<div class="wisp-chat-card"><div class="wisp-chat-card-header"><span>Plan</span><span class="wisp-chat-meta">${header}</span></div>${planTasks(states)}${actions ? `<div class="wisp-chat-card-actions">${actions}</div>` : ''}</div>`;
}

function chatView(state, { hover } = {}) {
	const project = 'Passwordless login';
	const host = 'mac-mini';
	const connected = contextBar({ project, host });
	const ask = message('user', '10:02', 'Replace password login with email magic links. Keep the old flow behind a flag for a week.');
	const go = message('user', '10:04', 'Go. Hold task 3 until the first two pass.');
	const planIntro = '<p>I read <code>src/auth</code> and the notes in <a>auth-notes.md</a>. Tasks 1 and 2 can run in parallel.</p>';

	switch (state) {
		case 'empty-m0':
			return `<div class="wisp-coordinator-chat">${contextBar({})}
				<div class="wisp-chat-empty"><h2>No host connected</h2><p>The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.</p></div>
				${composer({ placeholder: 'Not connected to a host', disabled: true })}</div>`;
		case 'empty-ready':
			return `<div class="wisp-coordinator-chat">${connected}
				<div class="wisp-chat-empty"><h2>Start with a goal</h2><p>Tell the coordinator what you want done. It plans the work, runs agents in their own worktrees on mac-mini, and brings the changes back for you to review.</p>
				<div class="wisp-chat-suggestions"><span class="wisp-chat-button is-secondary is-block">Research the auth code and take notes</span><span class="wisp-chat-button is-secondary is-block">Plan a change and wait for my go-ahead</span><span class="wisp-chat-button is-secondary is-block">Find and fix the failing tests</span></div></div>
				${composer({ placeholder: 'Message the coordinator', focused: true, account: 'Claude Max' })}</div>`;
		case 'conversation':
			return `<div class="wisp-coordinator-chat">${contextBar({ project, host, hover: hover ? 'project' : undefined })}
				<div class="wisp-chat-transcript is-scrolled">
					${ask}
					${message('coordinator', '10:03', planIntro + planCard([['review', 'needs review'], ['review', 'needs review'], ['queued', 'waits for 1 and 2']], 'approved 10:04'))}
					${go}
					${message('coordinator', '10:41', `<p>Both agents finished and their tests pass. Review the changes, then I'll start task 3.</p>
						<div class="wisp-chat-card">
							<div class="wisp-chat-result"><div class="wisp-chat-result-title"><span class="wisp-chat-dot is-review"></span>magic-link-api<span class="wisp-chat-meta">6 files <span class="wisp-chat-added">+214</span> <span class="wisp-chat-removed">−12</span></span></div><div><span class="wisp-chat-button is-secondary">Review changes</span></div></div>
							<div class="wisp-chat-result"><div class="wisp-chat-result-title"><span class="wisp-chat-dot is-review"></span>login-ui<span class="wisp-chat-meta">4 files <span class="wisp-chat-added">+131</span> <span class="wisp-chat-removed">−40</span></span></div><div><span class="wisp-chat-button is-secondary">Review changes</span></div></div>
						</div>`)}
				</div>
				${agentsStrip({ open: false, summary: '2 to review · 1 queued' })}
				${composer({ placeholder: 'Message the coordinator', account: 'Claude Max' })}</div>`;
		case 'running':
			return `<div class="wisp-coordinator-chat"><div class="wisp-chat-progress" role="progressbar" aria-label="Coordinator is working"></div>${connected}
				<div class="wisp-chat-transcript is-scrolled">
					${ask}
					${message('coordinator', '10:03', planIntro + planCard([['queued', 'ready'], ['queued', 'ready'], ['queued', 'waits for 1 and 2']], 'approved 10:04'))}
					${go}
					${message('coordinator', '10:04', `<p>Starting two agents on mac-mini. I'll hold task 3 until both pass their tests.</p>
						<div class="wisp-chat-event"><span class="wisp-chat-dot is-running"></span>Started <strong>magic-link-api</strong></div>
						<div class="wisp-chat-event"><span class="wisp-chat-dot is-running"></span>Started <strong>login-ui</strong></div>
						<div class="wisp-chat-working"><i></i><i></i><i></i>Working</div>`)}
				</div>
				${agentsStrip({ open: true, summary: '2 running · 1 queued', rows: [
					{ state: 'running', name: 'magic-link-api', task: 'Writing token store tests', status: '3m' },
					{ state: 'running', name: 'login-ui', task: 'Building the email form', status: '3m', hover: true },
					{ state: 'queued', name: 'password-flag', task: 'Waits for 1 and 2', status: 'Queued' },
				] })}
				${composer({ placeholder: 'Message the coordinator', stop: true, account: 'Claude Max' })}</div>`;
		case 'error':
			return `<div class="wisp-coordinator-chat">${contextBar({ project, host, hostState: 'offline' })}
				<div class="wisp-chat-transcript is-scrolled">
					${ask}
					${message('coordinator', '10:03', planIntro + planCard([['queued', 'ready'], ['queued', 'ready'], ['queued', 'waits for 1 and 2']], 'approved 10:04'))}
					${go}
					${message('coordinator', '10:04', '<p>Starting two agents on mac-mini. I\'ll hold task 3 until</p><div class="wisp-chat-interrupted">Interrupted: lost connection to mac-mini</div>')}
				</div>
				${agentsStrip({ open: false, summary: 'Last known: 2 running' })}
				<div class="wisp-chat-banner is-error" role="alert"><strong>Lost connection to mac-mini</strong>wisp retries every 10 seconds. Agents already running on the host keep going.<div class="wisp-chat-banner-actions"><span class="wisp-chat-button is-secondary">Retry now</span><span class="wisp-chat-button is-secondary">Show log</span></div></div>
				${composer({ placeholder: 'Reconnect to send messages', disabled: true, account: 'Claude Max' })}</div>`;
		case 'error-signin':
			return `<div class="wisp-coordinator-chat">${connected}
				<div class="wisp-chat-transcript">
					${ask}
					${message('coordinator', '10:02', `<div class="wisp-chat-banner is-error" style="margin:0"><strong>Claude Code isn't signed in on mac-mini</strong>Run <code>claude auth login</code> in a terminal on mac-mini, then retry.<div class="wisp-chat-banner-actions"><span class="wisp-chat-button is-secondary">Open terminal</span><span class="wisp-chat-button is-secondary">Retry</span></div></div>`)}
				</div>
				${composer({ placeholder: 'Message the coordinator', account: 'Claude Max' })}</div>`;
		case 'wide':
			return `<div class="wisp-coordinator-chat is-wide">${connected}
				<div class="wisp-chat-transcript">
					${ask}
					${message('coordinator', '10:03', planIntro + planCard([['queued', 'ready'], ['queued', 'ready'], ['queued', 'waits for 1 and 2']], 'approved 10:04'))}
					${go}
					${message('coordinator', '10:04', `<p>Starting two agents on mac-mini. I'll hold task 3 until both pass their tests.</p>
						<div class="wisp-chat-event"><span class="wisp-chat-dot is-running"></span>Started <strong>magic-link-api</strong></div>
						<div class="wisp-chat-event"><span class="wisp-chat-dot is-running"></span>Started <strong>login-ui</strong></div>`)}
				</div>
				${composer({ placeholder: 'Message the coordinator', account: 'Claude Max' })}</div>`;
		default:
			throw new Error(`unknown chat state ${state}`);
	}
}

function auxbar(inner, { style = '' } = {}) {
	return `<div class="wb-part wb-auxbar" style="${style}">
		<div class="wb-composite-title"><span class="wb-composite-pill">Coordinator</span><span class="wb-composite-actions"><span class="wb-icon-button">${shape.more()}</span><span class="wb-separator"></span><span class="wb-icon-button">${shape.maximize()}</span><span class="wb-icon-button">${shape.close()}</span></span></div>
		<div style="flex:1 1 auto;min-height:0">${inner}</div>
	</div>`;
}

// ---------------------------------------------------------------- workbench

function titlebar(command) {
	return `<div class="wb-part wb-titlebar">
		<span class="wb-traffic" style="left:13px;background:#ff5f57"></span><span class="wb-traffic" style="left:33px;background:#febc2e"></span><span class="wb-traffic" style="left:53px;background:#28c840"></span>
		<span class="wb-nav" style="left:420px">${shape.arrowLeft()}</span><span class="wb-nav" style="left:444px">${shape.arrowRight()}</span>
		<div class="wb-command-center">${esc(command)}</div>
		<div class="wb-title-actions"><span class="wb-icon-button">${shape.layoutCustom()}</span><span class="wb-icon-button">${shape.layoutLeft()}</span><span class="wb-icon-button">${shape.layoutPanel()}</span><span class="wb-icon-button">${shape.layoutRight()}</span></div>
	</div>`;
}

function activitybar({ items, standalone }) {
	const top = items.map((it, i) => `<span class="wb-activity-item${it.active ? ' is-active' : ''}" style="top:${4 + i * 44}px">${shape[it.shape]()}</span>`).join('');
	return `<div class="wb-part wb-activitybar${standalone ? ' is-standalone' : ''}">${top}<span class="wb-activity-item" style="top:789px">${shape.gear()}</span></div>`;
}

const fileType = (t) => `<span class="wb-filetype">${t}</span>`;
const twistie = (open) => `<span class="wb-twistie">${open ? shape.chevronDown() : shape.chevronRight()}</span>`;
const row = (depth, content, cls = '') => `<div class="wb-row ${cls}" style="padding-left:${4 + depth * 8}px">${content}</div>`;

function explorer({ sharedContext }) {
	const tree = [
		row(0, `${twistie(true)}src`),
		row(1, `${twistie(false)}api`),
		row(1, `${twistie(true)}auth`),
		row(2, `<span class="wb-twistie"></span>${fileType('TS')}login.ts`, 'is-selected'),
		row(2, `<span class="wb-twistie"></span>${fileType('TS')}session.ts`),
		row(1, `<span class="wb-twistie"></span>${fileType('TS')}app.ts`),
		row(0, `${twistie(false)}test`),
		row(0, `<span class="wb-twistie"></span>${fileType('{}')}package.json`),
		row(0, `<span class="wb-twistie"></span>${fileType('MD')}README.md`),
	].join('');
	const context = sharedContext ? `
		<div class="wb-pane-header is-divided">${twistie(true)}Shared context<span class="wb-count">3</span></div>
		<div class="wb-tree">
			${row(0, `<span class="wb-twistie"></span>${fileType('MD')}auth-notes.md<span class="wb-desc">research-auth</span>`)}
			${row(0, `<span class="wb-twistie"></span>${fileType('MD')}preferences.md<span class="wb-desc">you</span>`)}
			${row(0, `<span class="wb-twistie"></span>${fileType('MD')}test-instructions.md<span class="wb-desc">you</span>`)}
		</div>` : '';
	return `<div class="wb-part wb-sidebar">
		<div class="wb-composite-title"><h2>Explorer</h2><span class="wb-icon-button">${shape.more()}</span></div>
		<div class="wb-pane-header">${twistie(true)}tidepool</div>
		<div class="wb-tree">${tree}</div>
		<div style="flex:1"></div>
		${context}
		<div style="height:6px"></div>
	</div>`;
}

const code = {
	plain: [
		'<span class="kw">import</span> <span class="pu">{</span> <span class="va">createSession</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'./session\'</span><span class="pu">;</span>',
		'<span class="kw">import</span> <span class="pu">{</span> <span class="va">findUserByEmail</span><span class="pu">,</span> <span class="va">verifyPassword</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'../api/users\'</span><span class="pu">;</span>',
		'',
		'<span class="kw">export</span> <span class="st">interface</span> <span class="ty">LoginRequest</span> <span class="pu">{</span>',
		'  <span class="va">email</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">;</span>',
		'  <span class="va">password</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">;</span>',
		'<span class="pu">}</span>',
		'',
		'<span class="kw">export</span> <span class="st">async</span> <span class="st">function</span> <span class="fn">login</span><span class="pu">(</span><span class="va">request</span><span class="pu">:</span> <span class="ty">LoginRequest</span><span class="pu">):</span> <span class="ty">Promise</span><span class="pu">&lt;</span><span class="ty">string</span><span class="pu">&gt; {</span>',
		'  <span class="st">const</span> <span class="va">user</span> <span class="pu">=</span> <span class="kw">await</span> <span class="fn">findUserByEmail</span><span class="pu">(</span><span class="va">request</span><span class="pu">.</span><span class="va">email</span><span class="pu">);</span>',
		'  <span class="kw">if</span> <span class="pu">(!</span><span class="va">user</span><span class="pu">) {</span>',
		'    <span class="kw">throw</span> <span class="st">new</span> <span class="ty">Error</span><span class="pu">(</span><span class="str">\'Unknown email\'</span><span class="pu">);</span>',
		'  <span class="pu">}</span>',
		'  <span class="st">const</span> <span class="va">ok</span> <span class="pu">=</span> <span class="kw">await</span> <span class="fn">verifyPassword</span><span class="pu">(</span><span class="va">user</span><span class="pu">,</span> <span class="va">request</span><span class="pu">.</span><span class="va">password</span><span class="pu">);</span>',
		'  <span class="kw">if</span> <span class="pu">(!</span><span class="va">ok</span><span class="pu">) {</span>',
		'    <span class="kw">throw</span> <span class="st">new</span> <span class="ty">Error</span><span class="pu">(</span><span class="str">\'Wrong password\'</span><span class="pu">);</span>',
		'  <span class="pu">}</span>',
		'  <span class="kw">return</span> <span class="fn">createSession</span><span class="pu">(</span><span class="va">user</span><span class="pu">.</span><span class="va">id</span><span class="pu">);</span>',
		'<span class="pu">}</span>',
	],
	diff: [
		[' ', 1, '<span class="kw">import</span> <span class="pu">{</span> <span class="va">createSession</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'./session\'</span><span class="pu">;</span>'],
		['-', 2, '<span class="kw">import</span> <span class="pu">{</span> <span class="va">findUserByEmail</span><span class="pu">,</span> <span class="va">verifyPassword</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'../api/users\'</span><span class="pu">;</span>'],
		['+', 2, '<span class="kw">import</span> <span class="pu">{</span> <span class="va">sendMagicLink</span><span class="pu">,</span> <span class="va">verifyMagicLink</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'./magicLink\'</span><span class="pu">;</span>'],
		['+', 3, '<span class="kw">import</span> <span class="pu">{</span> <span class="va">flags</span> <span class="pu">}</span> <span class="kw">from</span> <span class="str">\'../flags\'</span><span class="pu">;</span>'],
		[' ', 4, ''],
		['-', 4, '<span class="kw">export</span> <span class="st">interface</span> <span class="ty">LoginRequest</span> <span class="pu">{</span>'],
		['-', 5, '  <span class="va">email</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">;</span>'],
		['-', 6, '  <span class="va">password</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">;</span>'],
		['-', 7, '<span class="pu">}</span>'],
		['+', 5, '<span class="kw">export</span> <span class="st">async</span> <span class="st">function</span> <span class="fn">startLogin</span><span class="pu">(</span><span class="va">email</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">):</span> <span class="ty">Promise</span><span class="pu">&lt;</span><span class="ty">void</span><span class="pu">&gt; {</span>'],
		['+', 6, '  <span class="kw">await</span> <span class="fn">sendMagicLink</span><span class="pu">(</span><span class="va">email</span><span class="pu">);</span>'],
		['+', 7, '<span class="pu">}</span>'],
		[' ', 8, ''],
		['+', 9, '<span class="kw">export</span> <span class="st">async</span> <span class="st">function</span> <span class="fn">finishLogin</span><span class="pu">(</span><span class="va">token</span><span class="pu">:</span> <span class="ty">string</span><span class="pu">):</span> <span class="ty">Promise</span><span class="pu">&lt;</span><span class="ty">string</span><span class="pu">&gt; {</span>'],
		['+', 10, '  <span class="st">const</span> <span class="va">user</span> <span class="pu">=</span> <span class="kw">await</span> <span class="fn">verifyMagicLink</span><span class="pu">(</span><span class="va">token</span><span class="pu">);</span>'],
		['+', 11, '  <span class="kw">return</span> <span class="fn">createSession</span><span class="pu">(</span><span class="va">user</span><span class="pu">.</span><span class="va">id</span><span class="pu">);</span>'],
		['+', 12, '<span class="pu">}</span>'],
		[' ', 13, ''],
		[' ', 14, '<span class="co">// Old flow, removed after the flag has been off for a week.</span>'],
		[' ', 15, '<span class="kw">export</span> <span class="st">async</span> <span class="st">function</span> <span class="fn">login</span><span class="pu">(</span><span class="va">request</span><span class="pu">:</span> <span class="ty">PasswordLogin</span><span class="pu">):</span> <span class="ty">Promise</span><span class="pu">&lt;</span><span class="ty">string</span><span class="pu">&gt; {</span>'],
		['+', 16, '  <span class="kw">if</span> <span class="pu">(!</span><span class="va">flags</span><span class="pu">.</span><span class="va">passwordLogin</span><span class="pu">) {</span>'],
		['+', 17, '    <span class="kw">throw</span> <span class="st">new</span> <span class="ty">Error</span><span class="pu">(</span><span class="str">\'Password login is off\'</span><span class="pu">);</span>'],
		['+', 18, '  <span class="pu">}</span>'],
	],
};

function editor({ mode }) {
	if (mode === 'review') {
		const lines = code.diff.map(([sign, n, html]) => `<div class="wb-line ${sign === '+' ? 'is-inserted' : sign === '-' ? 'is-removed' : ''}"><span class="ln">${n}</span><span class="sign">${sign === ' ' ? '' : sign}</span><span class="src" style="padding-left:6px">${html}</span></div>`).join('');
		return `<div class="wb-part wb-editor">
			<div class="wb-tabs"><div class="wb-tab is-active">Review: magic-link-api<span class="wb-close">${shape.close()}</span></div><div class="wb-tab">${fileType('TS')}login.ts</div></div>
			<div class="wb-review-bar"><span class="wisp-chat-dot is-review"></span><strong>magic-link-api</strong><span class="wisp-chat-meta">6 files <span class="wisp-chat-added">+214</span> <span class="wisp-chat-removed">−12</span> · tests passed · worktree on mac-mini</span><span class="wisp-chat-button is-secondary">Request changes</span><span class="wisp-chat-button">Accept</span></div>
			<div class="wb-diff-file">${shape.chevronDown()}<span>src/auth/login.ts</span><span class="wb-desc"><span class="wisp-chat-added">+13</span> <span class="wisp-chat-removed">−5</span></span></div>
			<div class="wb-code">${lines}</div>
			<div class="wb-diff-file">${shape.chevronRight()}<span>src/auth/magicLink.ts</span><span class="wb-desc"><span class="wisp-chat-added">+96</span></span></div>
		</div>`;
	}
	const lines = code.plain.map((html, i) => `<div class="wb-line${i === 9 ? ' is-current' : ''}"><span class="ln">${i + 1}</span><span class="src">${html}</span></div>`).join('');
	return `<div class="wb-part wb-editor">
		<div class="wb-tabs"><div class="wb-tab is-active">${fileType('TS')}login.ts<span class="wb-close">${shape.close()}</span></div><div class="wb-tab">${fileType('TS')}session.ts</div></div>
		<div class="wb-breadcrumbs">src ${shape.chevronRight()} auth ${shape.chevronRight()} ${fileType('TS')} login.ts</div>
		<div class="wb-code">${lines}</div>
	</div>`;
}

function panel(lines) {
	return `<div class="wb-part wb-panel">
		<div class="wb-panel-title"><span class="wb-panel-tab">Problems</span><span class="wb-panel-tab">Output</span><span class="wb-panel-tab is-active">Terminal</span>
			<span style="flex:1"></span><span class="wb-icon-button" style="width:auto;padding:0 6px;font-size:12px">zsh</span><span class="wb-icon-button">${shape.plus()}</span><span class="wb-icon-button">${shape.more()}</span><span class="wb-icon-button">${shape.close()}</span></div>
		<div class="wb-terminal">${lines}</div>
	</div>`;
}

function statusbar({ left, right }) {
	return `<div class="wb-part wb-statusbar"><div class="wb-status-group">${left.join('')}</div><div class="wb-status-group">${right.join('')}</div></div>`;
}

const status = (html, cls = '') => `<span class="wb-status-item ${cls}">${html}</span>`;
const problems = status(`${shape.errorCount()}0 ${shape.warningCount()}0`);
const editorStatus = ['Ln 10, Col 3', 'Spaces: 2', 'UTF-8', 'LF', 'TypeScript'].map((t) => status(t));
const prompt = (cmd) => `<span class="t-path">~/code/tidepool</span> <span class="t-dim">main</span> % ${cmd}`;

function workbench(stage) {
	const m4 = stage === 'm4';
	const terminal = m4
		? [prompt('npm test -- auth'), '<span class="t-ok">PASS</span>  test/login.test.ts', '<span class="t-ok">PASS</span>  test/session.test.ts', '', 'Tests:       14 passed, 14 total', prompt('<span class="wb-cursor"></span>')].join('\n')
		: [prompt('npm test -- auth'), '<span class="t-ok">PASS</span>  test/login.test.ts', '', 'Tests:       9 passed, 9 total', prompt('<span class="wb-cursor"></span>')].join('\n');
	return `<div class="wb has-panel">
		${titlebar('tidepool')}
		${activitybar({ items: [{ shape: 'files', active: true }, { shape: 'search' }, { shape: 'scm' }, { shape: 'extensions' }] })}
		${explorer({ sharedContext: m4 })}
		${editor({ mode: m4 ? 'review' : 'file' })}
		${panel(terminal)}
		${auxbar(chatView(m4 ? 'conversation' : 'empty-m0'))}
		${statusbar({
			left: m4 ? [status('mac-mini'), status(`${shape.branch()}main`), problems] : [status(`${shape.branch()}main`), problems],
			right: m4 ? [status('2 agents to review'), status('Claude Max 38%'), ...editorStatus] : editorStatus,
		})}
	</div>`;
}

// ---------------------------------------------------------------- option B for #14

function optionB() {
	const projectRow = (name, desc, active) => `<div class="wb-row${active ? ' is-selected' : ''}" style="padding-left:4px">${twistie(active)}<strong style="font-weight:600">${name}</strong><span class="wb-desc">${desc}</span></div>`;
	const agentRow = (state, name, desc) => `<div class="wb-row" style="padding-left:28px"><span class="wisp-chat-dot is-${state}"></span><span style="margin-left:4px">${name}</span><span class="wb-desc">${desc}</span></div>`;
	return `<div class="wb">
		${titlebar('tidepool')}
		${activitybar({ items: [{ shape: 'projects', active: true }, { shape: 'files' }, { shape: 'search' }, { shape: 'scm' }, { shape: 'extensions' }] })}
		<div class="wb-part wb-sidebar">
			<div class="wb-composite-title"><h2>Projects</h2><span class="wb-composite-actions"><span class="wb-icon-button">${shape.plus()}</span><span class="wb-icon-button">${shape.more()}</span></span></div>
			<div class="wb-tree">
				${projectRow('Passwordless login', 'mac-mini', true)}
				${agentRow('running', 'magic-link-api', '3m')}
				${agentRow('running', 'login-ui', '3m')}
				${agentRow('queued', 'password-flag', 'queued')}
				${projectRow('Nightly upkeep', 'mac-mini · idle')}
				${projectRow('Search migration', 'MacBook · 3 to review')}
			</div>
		</div>
		<div class="wb-part wb-editor" style="width:784px">
			<div class="wb-tabs"><div class="wb-tab is-active">Coordinator: Passwordless login<span class="wb-close">${shape.close()}</span></div><div class="wb-tab">${fileType('TS')}login.ts</div></div>
			<div style="flex:1 1 auto;min-height:0">${chatView('wide')}</div>
		</div>
		<div class="wb-part wb-sidebar is-right" style="left:1140px;width:296px">
			<div class="wb-composite-title"><h2>Explorer</h2><span class="wb-icon-button">${shape.more()}</span></div>
			<div class="wb-pane-header">${twistie(true)}tidepool</div>
			<div class="wb-tree">
				${row(0, `${twistie(true)}src`)}${row(1, `${twistie(false)}api`)}${row(1, `${twistie(true)}auth`)}
				${row(2, `<span class="wb-twistie"></span>${fileType('TS')}login.ts`)}${row(2, `<span class="wb-twistie"></span>${fileType('TS')}session.ts`)}
				${row(1, `<span class="wb-twistie"></span>${fileType('TS')}app.ts`)}${row(0, `${twistie(false)}test`)}
				${row(0, `<span class="wb-twistie"></span>${fileType('{}')}package.json`)}
			</div>
			<div style="flex:1"></div>
			<div class="wb-pane-header is-divided">${twistie(true)}Changes: magic-link-api<span class="wb-count">6</span></div>
			<div class="wb-tree">
				${row(0, `<span class="wb-twistie"></span>${fileType('TS')}login.ts<span class="wb-deco wb-modified">M</span>`)}
				${row(0, `<span class="wb-twistie"></span>${fileType('TS')}magicLink.ts<span class="wb-deco wb-added">A</span>`)}
				${row(0, `<span class="wb-twistie"></span>${fileType('TS')}tokenStore.ts<span class="wb-deco wb-added">A</span>`)}
			</div>
			<div class="wb-pane-header is-divided">${twistie(false)}Shared context<span class="wb-count">3</span></div>
			<div style="height:6px"></div>
		</div>
		${statusbar({ left: [status('mac-mini'), status(`${shape.branch()}main`), problems], right: [status('2 agents running'), status('Claude Max 38%')] })}
	</div>`;
}

// ---------------------------------------------------------------- state boards


// Markers are [number, side, top]: side 'l' or 'r' of the first frame, top in frame pixels.
const boards = {
	empty: {
		title: 'Chat state 1 of 4: Empty',
		sub: 'M0 ships the first two columns (#12). The third is the same state once a host is connected.',
		columns: [
			['dark-modern', 'Dark Modern · no host (M0)', 'empty-m0'],
			['light-modern', 'Light Modern · no host (M0)', 'empty-m0'],
			['dark-modern', 'Dark Modern · connected, no messages', 'empty-ready'],
		],
		markers: [[1, 'l', 6], [2, 'l', 36], [3, 'r', 36], [4, 'l', 340], [5, 'l', 737], [6, 'r', 757]],
		notes: [
			['Container', 'View container <code>wisp.coordinator</code>, titled Coordinator, the default container in the secondary side bar. It holds one view, <code>wisp.coordinatorChat</code>. The view root carries the class <code>wisp-coordinator-chat</code>.'],
			['Project', 'Plain text "No project" in M0. From M1 it is the project switcher.'],
			['Host', 'Plain text "Not connected" with a hollow dot in M0. From M1 it shows the host and its state, and opens the host menu.'],
			['Empty-state copy', 'Heading <b>No host connected</b>. Body: <b>The coordinator runs on a host: this Mac, or another machine running wispd. You can chat once wisp connects to one.</b>'],
			['Composer, disabled', 'Placeholder <b>Not connected to a host</b>. The textarea is read-only with <code>aria-disabled="true"</code>, so it stays focusable and announces why.'],
			['Send', 'A text button with the <code>button.*</code> tokens, at 0.4 opacity while disabled.'],
		],
		extra: '<h3>Connected, no messages</h3><div>Heading <b>Start with a goal</b>. Up to three suggestions fill the composer when clicked; they do not send. The composer takes focus when the view opens.</div>',
	},
	conversation: {
		title: 'Chat state 2 of 4: Conversation',
		sub: 'Nothing is running. Two agents finished and wait for review; the third is queued.',
		columns: [
			['dark-modern', 'Dark Modern', 'conversation', { hover: true }],
			['light-modern', 'Light Modern', 'conversation'],
			['hc-dark', 'Dark High Contrast', 'conversation'],
		],
		markers: [[1, 'l', 36], [2, 'r', 250], [3, 'l', 410], [4, 'l', 474], [5, 'r', 572], [6, 'l', 674], [7, 'l', 737]],
		notes: [
			['Context bar', 'Project switcher (a Quick Pick of projects, then New project) and host status (the host menu). Shown hovered.'],
			['Plan card', 'Numbered tasks with the agent and state of each. While a plan waits for approval, the card ends with Run plan and Change plan.'],
			['Your messages', '<code>input.background</code> with <code>input.border</code>, so your words keep the look of the composer.'],
			['Coordinator messages', 'Plain text on the view background. Links use <code>textLink.foreground</code>; inline code uses <code>textPreformat.*</code>.'],
			['Result card', 'One row per finished agent, with its diff stat. Review changes opens that agent\'s worktree diff in the editor area.'],
			['Agents strip', 'The live subagent list, collapsed to a summary when nothing runs. Click or press Enter to expand it.'],
			['Composer', 'Enter sends; Shift+Enter adds a line. The footer holds the coordinator\'s account picker (M2).'],
		],
		extra: '<h3>Scrolling</h3><div>The transcript stays pinned to the newest message unless you scroll up. A <code>scrollbar.shadow</code> edge shows there is more above.</div>',
	},
	running: {
		title: 'Chat state 3 of 4: Agent running',
		sub: 'The coordinator\'s turn is streaming, and two subagents run on the host.',
		columns: [
			['dark-modern', 'Dark Modern', 'running'],
			['light-modern', 'Light Modern', 'running'],
			['hc-dark', 'Dark High Contrast', 'running'],
		],
		markers: [[1, 'l', 22], [2, 'l', 520], [3, 'l', 640], [4, 'r', 757]],
		notes: [
			['Progress', 'A 2px <code>progressBar.background</code> bar at the top of the view while the coordinator\'s own turn runs. It follows reduced motion.'],
			['Streaming message', 'Text streams in, and "Working" ends the message until the turn finishes. Agent starts are logged as events; live status is not repeated here.'],
			['Agents strip, open', 'The one place with live status: state, name, current step, elapsed time. A row opens the agent\'s detail; hovering a row shows Stop agent.'],
			['Stop', 'Replaces Send while the turn runs; also <code>Cmd+Esc</code>. It ends the coordinator\'s turn only, and agents keep running. Typing stays enabled: Enter queues the message and sends it when the turn ends.'],
		],
		extra: '<h3>Screen readers</h3><div>Streaming text is never announced token by token. A finished message, an agent start, and an agent finish are each announced once, politely.</div>',
	},
	error: {
		title: 'Chat state 4 of 4: Error',
		sub: 'Connection problems use the banner. A problem with one turn appears inline, where it happened.',
		columns: [
			['dark-modern', 'Dark Modern · lost connection', 'error'],
			['light-modern', 'Light Modern · lost connection', 'error'],
			['hc-dark', 'Dark High Contrast · not signed in (inline)', 'error-signin'],
		],
		markers: [[1, 'r', 36], [2, 'l', 526], [3, 'l', 561], [4, 'l', 637], [5, 'l', 737]],
		notes: [
			['Host state', 'The host shows offline in <code>errorForeground</code>. The status bar item switches to <code>statusBarItem.errorBackground</code>.'],
			['Interrupted turn', 'Marked where it stopped. Text already received stays.'],
			['Agents strip', 'Shows the last known state and says so.'],
			['Banner', '<code>role="alert"</code>, with <code>inputValidation.errorBackground</code> and <code>inputValidation.errorBorder</code>. It says what happens next and offers Retry now and Show log.'],
			['Composer', 'Disabled until the host is back. A draft you already typed is kept.'],
		],
		extra: '<h3>Inline errors</h3><div>A turn that fails for its own reason shows an error card in the transcript with the fix. The cases are: not signed in (third column, <code>claude auth login</code> from decision 0004), usage limit reached, the no-write guard stopping the turn, and a crashed CLI. The spec lists the copy.</div>',
	},
};

function board(key) {
	const b = boards[key];
	const columns = b.columns.map(([theme, caption, state, options], i) => {
		const markers = i === 0 ? b.markers.map(([n, side, top]) => `<span class="marker is-pinned is-${side}" style="left:${side === 'l' ? -34 : 306}px;top:${top}px">${n}</span>`).join('') : '';
		return `<div class="board-column"><div class="board-caption">${caption}</div><div class="board-frame theme-${theme}">${auxbar(chatView(state, options), { style: 'height:800px' })}${markers}</div></div>`;
	}).join('');
	const notes = b.notes.map(([title, text], i) => `<div class="board-note"><span class="marker">${i + 1}</span><div><b>${title}.</b> ${text}</div></div>`).join('');
	return `<div class="board"><div class="board-header"><h1>${b.title}</h1><span class="board-sub">${b.sub}</span></div>
		<div class="board-columns">${columns}</div>
		<div class="board-notes">${notes}${b.extra ?? ''}</div></div>`;
}

// ---------------------------------------------------------------- mount

const theme = params.get('theme') ?? 'dark-modern';
let html;
if (page === 'workbench') {
	document.body.className = `theme-${theme}`;
	html = workbench(params.get('stage') ?? 'm4');
} else if (page === 'option-b') {
	document.body.className = `theme-${theme}`;
	html = optionB();
} else if (page === 'state') {
	html = board(params.get('state') ?? 'empty');
} else {
	throw new Error(`unknown page ${page}`);
}
document.body.innerHTML = html;
document.body.dataset.ready = 'true';
