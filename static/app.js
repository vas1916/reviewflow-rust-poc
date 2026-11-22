'use strict';
// ReviewFlow web app. Plain JS, no build step. All text is inserted with textContent.

const $ = id => document.getElementById(id);
const state = {
  token: '', health: null, me: null, reviews: [], filter: 'all', selectedId: null,
  audit: [], dirty: false, sendBackOpen: false, timer: null,
};

// ---------- Labels ----------
const STATUS = {
  SUBMITTED:       { label: 'Summarizing',            tone: 'busy' },
  ANALYSIS_FAILED: { label: 'Summary failed',         tone: 'bad' },
  EXTRACTED:       { label: 'Ready for approval',     tone: 'action' },
  NEEDS_REVIEW:    { label: 'Ready, check flags',     tone: 'warn' },
  RETURNED:        { label: 'Sent back',              tone: 'muted' },
  APPROVED:        { label: 'Approved',               tone: 'busy' },
  STAGED:          { label: 'Queued for Workday',     tone: 'busy' },
  DELIVERY_FAILED: { label: 'Workday delivery failed', tone: 'bad' },
  MOCK_RECEIVED:   { label: 'In Workday (simulated)', tone: 'good' },
  DELIVERED:       { label: 'In Workday',             tone: 'good' },
};
const FILTERS = [
  { key: 'all',       label: 'All',                 match: () => true },
  { key: 'approval',  label: 'Waiting for approval', match: s => ['EXTRACTED', 'NEEDS_REVIEW'].includes(s) },
  { key: 'progress',  label: 'In progress',          match: s => ['SUBMITTED', 'APPROVED', 'STAGED'].includes(s) },
  { key: 'attention', label: 'Needs attention',      match: s => ['ANALYSIS_FAILED', 'DELIVERY_FAILED'].includes(s) },
  { key: 'done',      label: 'In Workday',           match: s => s === 'MOCK_RECEIVED' || s === 'DELIVERED' },
  { key: 'returned',  label: 'Sent back',            match: s => s === 'RETURNED' },
];
const EVENTS = {
  SUBMITTED: 'Review received',
  EXTRACTED: 'Summarized by the assistant',
  NEEDS_REVIEW: 'Summarized, with items to check',
  SUBMITTED_RETRY_SCHEDULED: 'Summary attempt failed, retry scheduled',
  ANALYSIS_FAILED: 'Summary failed after several attempts',
  APPROVED: 'Approved',
  APPROVED_WITH_EDITS: 'Approved with edits',
  RETURNED: 'Sent back to the employee',
  RETRY_REQUESTED: 'Retry requested',
  STAGED: 'Prepared for Workday',
  STAGED_RETRY_SCHEDULED: 'Workday delivery failed, retry scheduled',
  DELIVERY_FAILED: 'Workday delivery failed after several attempts',
  MOCK_RECEIVED_NOT_WORKDAY: 'Delivered to simulated Workday',
  DELIVERED_TO_WORKDAY: 'Delivered to Workday',
  DELIVERY_OUTCOME_UNKNOWN: 'Delivery interrupted; outcome unknown',
};
const SECTIONS = [
  ['achievements', 'Achievements'],
  ['goals', 'Goals'],
  ['improvements', 'Development areas'],
];

// ---------- Helpers ----------
function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') node.className = v;
    else if (k === 'text') node.textContent = v;
    else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) if (c !== null && c !== undefined && c !== false) node.append(c);
  return node;
}
function pill(status) {
  const s = STATUS[status] || { label: status, tone: 'muted' };
  return el('span', { class: `pill ${s.tone}`, text: s.label });
}
function sourceLabel(r) {
  return r.source_system.startsWith('microsoft-forms') ? 'Teams form' : r.source_system === 'form' ? 'Web form' : r.source_system;
}
function ago(iso) {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(iso).toLocaleDateString();
}
function when(iso) { return new Date(iso).toLocaleString(); }
function toast(message, bad = false) {
  const t = $('toast');
  t.textContent = message; t.className = `toast${bad ? ' bad' : ''}`; t.hidden = false;
  clearTimeout(toast.timer); toast.timer = setTimeout(() => { t.hidden = true; }, 4500);
}
function store(key, value) { try { value ? localStorage.setItem(key, value) : localStorage.removeItem(key); } catch { /* ignore */ } }
function recall(key) { try { return localStorage.getItem(key) || ''; } catch { return ''; } }

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  // With Entra ID sign-in the platform's session cookie authenticates; no token needed.
  if (state.token) headers.set('Authorization', `Bearer ${state.token}`);
  if (options.body) headers.set('Content-Type', 'application/json');
  const res = await fetch(path, { ...options, headers, cache: 'no-store' });
  if (!res.ok) {
    let detail; try { detail = (await res.json()).error; } catch { detail = res.statusText; }
    const err = new Error(detail || `Request failed (${res.status})`); err.status = res.status; throw err;
  }
  if (res.headers.get('content-type')?.includes('text/csv')) return res.blob();
  return res.json();
}
async function act(fn) { try { await fn(); } catch (e) { toast(e.message, true); } }
const agentOn = () => !!state.health?.agent?.enabled;
const workdayLive = () => state.health?.workday_mode === 'soap';
const entra = () => state.health?.auth_mode === 'entra';
const verifiedUser = () => !!state.me?.user?.verified;
const isHr = () => state.me?.user ? !!state.me.user.hr : true;
const autoDeliver = () => !!state.health?.agent?.auto_deliver;

// ---------- Health chips ----------
async function loadHealth() {
  try { state.health = await (await fetch('/healthz', { cache: 'no-store' })).json(); } catch { state.health = null; }
  const h = state.health, chips = $('chips');
  chips.replaceChildren();
  if (!h) { chips.append(el('span', { class: 'chip bad', text: 'Server unreachable' })); return; }
  const ai = { rules: 'Rules (no AI)', ollama: 'Local AI', azure: 'Azure OpenAI' }[h.ai_mode] || h.ai_mode;
  chips.append(
    el('span', { class: 'chip', text: `Summaries: ${ai}` }),
    el('span', { class: `chip ${h.agent?.enabled ? 'good' : ''}`, text: h.agent?.enabled ? 'Assistant: on' : 'Assistant: off (manual)' }),
    el('span', { class: 'chip', text: h.teams_notifications ? 'Teams alerts: on' : 'Teams alerts: off' }),
    workdayLive() ? el('span', { class: 'chip good', text: 'Workday: live' }) : el('span', { class: 'chip warn', text: 'Workday: simulated' }),
  );
  if (workdayLive()) {
    $('footer').textContent = 'Approved reviews are sent to your Workday tenant. Every step is recorded in the review history.';
  }
}

// ---------- Sign in ----------
async function loadMe() {
  try { state.me = await api('/api/v1/me'); } catch { state.me = null; }
  const u = state.me?.user;
  $('user').hidden = !u?.verified;
  if (u) $('user').textContent = `${u.name}${u.verified ? (u.hr ? ' · HR' : ' · Manager') : ''}`;
}
async function enterApp() {
  $('signin').hidden = true; $('app').hidden = false;
  $('export').hidden = !isHr();
  // Never offer test submissions when approvals go to a real Workday tenant.
  $('test-panel').hidden = workdayLive();
  $('signout').textContent = entra() ? 'Sign out of Microsoft' : 'Sign out';
  await refresh();
  routeFromHash();
  clearInterval(state.timer);
  state.timer = setInterval(() => act(refresh), 5000);
}
async function signIn(token, remember) {
  state.token = token;
  try {
    await api('/api/v1/reviews');
  } catch (e) {
    if (e.status === 401) {
      // Could be the employee code: it can submit but not see the queue.
      state.token = '';
      throw new Error('That access code cannot open the review queue. Use REVIEWER_API_TOKEN.');
    }
    throw e;
  }
  store('rf-token', remember ? token : '');
  await loadMe();
  await enterApp();
}
async function startEntra() {
  $('signin-form').hidden = true;
  $('signin-help').textContent = 'Sign in with your work account. You need the ReviewFlow Reviewer or HR role.';
  await loadMe();
  if (state.me?.user) { await enterApp(); return; }
  if (state.me?.error) {
    $('signin-error').textContent = state.me.error; $('signin-error').hidden = false;
    $('entra-signin').textContent = 'Sign in with a different account';
    $('entra-signin').href = '/.auth/logout?post_logout_redirect_uri=/';
  }
  $('entra-signin').hidden = false;
}
function signOut() {
  clearInterval(state.timer);
  if (entra()) { location.href = '/.auth/logout?post_logout_redirect_uri=/'; return; }
  state.token = ''; store('rf-token', '');
  $('token').value = ''; $('app').hidden = true; $('signin').hidden = false; $('user').hidden = true;
}

// ---------- Queue ----------
async function refresh() {
  state.reviews = await api('/api/v1/reviews');
  $('updated').textContent = `Updated ${new Date().toLocaleTimeString()}`;
  renderTiles(); renderQueue();
  if (state.selectedId) {
    const fresh = state.reviews.find(r => r.id === state.selectedId);
    const shown = $('detail').dataset.version;
    // Re-render only when the review changed and the reviewer isn't mid-edit.
    if (fresh && shown !== `${fresh.id}:${fresh.updated_at}` && !state.dirty && !state.sendBackOpen) await select(fresh.id, false);
  }
}
function renderTiles() {
  const tiles = $('tiles'); tiles.replaceChildren();
  for (const f of FILTERS) {
    const count = state.reviews.filter(r => f.match(r.status)).length;
    tiles.append(el('button', {
      class: `tile${state.filter === f.key ? ' active' : ''}${f.key === 'attention' && count ? ' alert' : ''}`,
      role: 'tab', 'aria-selected': String(state.filter === f.key),
      onclick: () => { state.filter = f.key; renderTiles(); renderQueue(); },
    }, el('span', { class: 'tile-count', text: String(count) }), el('span', { class: 'tile-label', text: f.label })));
  }
}
function renderQueue() {
  const f = FILTERS.find(x => x.key === state.filter);
  $('queue-title').textContent = f.label === 'All' ? 'All reviews' : f.label;
  const list = $('queue'); list.replaceChildren();
  const rows = state.reviews.filter(r => f.match(r.status));
  if (!rows.length) { list.append(el('li', { class: 'muted queue-empty', text: 'Nothing here right now.' })); return; }
  for (const r of rows) {
    list.append(el('li', {}, el('button', {
      class: `queue-item${r.id === state.selectedId ? ' selected' : ''}`,
      onclick: () => act(() => select(r.id)),
    },
      el('div', { class: 'qi-top' }, el('strong', { text: r.employee_id }), pill(r.status)),
      el('div', { class: 'qi-sub muted small', text: `${r.review_period} · ${sourceLabel(r)} · ${ago(r.created_at)}` }),
    )));
  }
}

// ---------- Detail ----------
async function select(id, pushHash = true) {
  const [review, audit] = await Promise.all([api(`/api/v1/reviews/${id}`), api(`/api/v1/reviews/${id}/audit`)]);
  state.selectedId = id; state.audit = audit; state.dirty = false; state.sendBackOpen = false;
  if (pushHash && location.hash !== `#review-${id}`) history.replaceState(null, '', `#review-${id}`);
  renderQueue(); renderDetail(review);
}
function routeFromHash() {
  const m = location.hash.match(/^#review-([0-9a-f-]{36})$/);
  if (m) act(() => select(m[1], false));
}

function banner(r) {
  const busy = (text) => el('div', { class: 'banner busy' }, el('span', { class: 'spinner', 'aria-hidden': 'true' }), el('span', { text }));
  switch (r.status) {
    case 'SUBMITTED':
      if (!agentOn()) return el('div', { class: 'banner' }, 'The assistant is off. Use "Summarize now" below.');
      return busy(r.attempts ? `The last summary attempt failed (${r.last_error}). Retrying automatically.` : 'The assistant is summarizing this review.');
    case 'ANALYSIS_FAILED':
      return el('div', { class: 'banner bad' }, `The assistant couldn't summarize this review: ${r.last_error || 'unknown error'}. Retry, or send it back to the employee.`);
    case 'EXTRACTED':
      return el('div', { class: 'banner action' }, 'Check the summary against the employee\'s words, edit if needed, then approve or send back.');
    case 'NEEDS_REVIEW':
      return el('div', { class: 'banner warn' }, 'The assistant flagged items to check. Read the flags before approving.');
    case 'RETURNED':
      return el('div', { class: 'banner' }, `Sent back to the employee: "${r.return_reason}". They can resubmit through the Teams form.`);
    case 'APPROVED':
    case 'STAGED':
      if (agentOn() && autoDeliver()) return busy(r.attempts ? `${r.last_error}. Retrying automatically.` : 'Approved. The assistant is sending it to Workday.');
      return el('div', { class: 'banner' }, 'Approved. Use the manual steps below to send it to Workday.');
    case 'DELIVERY_FAILED':
      if ((r.last_error || '').startsWith('Outcome unknown')) {
        return el('div', { class: 'banner bad' }, `${r.last_error}. Look for this review in Workday first. Only retry if it isn't there, or it will be created twice.`);
      }
      return el('div', { class: 'banner bad' }, `${r.last_error || 'Workday delivery failed'}. Retry when the problem is fixed.`);
    case 'MOCK_RECEIVED':
      return el('div', { class: 'banner good' }, `Delivered to the simulated Workday receiver (${r.workday_receipt_id || 'no receipt'}). No real Workday tenant was called.`);
    case 'DELIVERED':
      return el('div', { class: 'banner good' }, `Workday accepted this review. Workday reference: ${r.workday_receipt_id || 'not returned'}.`);
    default: return null;
  }
}

function renderDetail(r) {
  const d = $('detail');
  d.dataset.version = `${r.id}:${r.updated_at}`;
  d.replaceChildren();
  const editable = ['EXTRACTED', 'NEEDS_REVIEW'].includes(r.status);
  const analysis = r.approved || r.extracted;

  d.append(
    el('div', { class: 'detail-head' },
      el('div', {},
        el('h2', { text: `${r.employee_id} · ${r.review_period}` }),
        el('div', { class: 'muted small', text: [sourceLabel(r), r.template_id, r.submitted_by && `from ${r.submitted_by}`, r.manager_email && `approver ${r.manager_email}`, `received ${when(r.created_at)}`].filter(Boolean).join(' · ') }),
      ),
      pill(r.status),
    ),
    banner(r),
  );

  if (analysis) d.append(editable ? editor(r, analysis) : readOnly(r, analysis));

  // Actions
  const actions = el('div', { class: 'actions' });
  if (editable) {
    if (!verifiedUser()) {
      actions.append(el('label', { class: 'approver' }, 'Your name',
        el('input', { id: 'approver', value: recall('rf-approver'), placeholder: 'e.g. Priya Shah', maxlength: '100' })));
    }
    actions.append(
      el('button', { class: 'primary', onclick: () => act(() => approve(r)) }, 'Approve and send to Workday'),
      el('button', { onclick: () => openSendBack(r) }, 'Send back'),
    );
  }
  if (r.status === 'ANALYSIS_FAILED') {
    actions.append(
      el('button', { class: 'primary', onclick: () => act(() => post(r, 'retry', 'Queued for another attempt')) }, 'Retry summary'),
      el('button', { onclick: () => openSendBack(r) }, 'Send back'),
    );
  }
  if (r.status === 'DELIVERY_FAILED') {
    actions.append(el('button', { class: 'primary', onclick: () => act(() => post(r, 'retry', 'Queued for another delivery attempt')) }, 'Retry delivery'));
  }
  if (actions.childElementCount) d.append(actions);
  d.append(el('div', { id: 'sendback-slot' }));

  // Manual steps when the assistant is off (or auto-delivery is off).
  const manual = el('div', { class: 'manual' });
  if (r.status === 'SUBMITTED' && !agentOn()) manual.append(el('button', { onclick: () => act(() => post(r, 'analyze', 'Summarized')) }, 'Summarize now'));
  if (r.status === 'APPROVED' && !(agentOn() && autoDeliver())) manual.append(el('button', { onclick: () => act(() => post(r, 'stage', 'Prepared for Workday')) }, 'Prepare for Workday'));
  if (r.status === 'STAGED' && !(agentOn() && autoDeliver())) {
    manual.append(el('button', { onclick: () => act(() => post(r, 'deliver', workdayLive() ? 'Delivered to Workday' : 'Delivered to simulated Workday')) },
      workdayLive() ? 'Send to Workday' : 'Send to Workday (simulated)'));
  }
  if (manual.childElementCount) d.append(el('h3', { class: 'section-title', text: 'Manual steps' }), manual);

  // Original text and history
  d.append(
    el('details', { class: 'original' }, el('summary', { text: 'Original review, as the employee wrote it' }), el('pre', { text: r.raw_text })),
    el('h3', { class: 'section-title', text: 'History' }),
    el('ol', { class: 'timeline' }, state.audit.map(e => el('li', {},
      el('span', { class: 'tl-what', text: EVENTS[e.event] || e.event }),
      el('span', { class: 'muted small', text: `${when(e.at)} · ${e.actor}` }),
    ))),
  );
}

function flagsList(a) {
  if (!a.risk_flags?.length) return null;
  return el('div', { class: 'flags' }, el('h3', { text: 'Items to check' }), el('ul', {}, a.risk_flags.map(f => el('li', { text: f }))));
}

function approvalNote(r) {
  if (!r.approved || !r.approved_by) return null;
  const event = state.audit.find(e => e.event === 'APPROVED' || e.event === 'APPROVED_WITH_EDITS');
  const signedIn = !!event?.actor.endsWith('(Entra ID)');
  return el('p', { class: 'muted small', text: signedIn
    ? `Approved by ${r.approved_by} (signed in with Microsoft)`
    : `Approved by ${r.approved_by} (name entered at approval; not verified in this demo)` });
}

function readOnly(r, a) {
  return el('div', { class: 'analysis' },
    approvalNote(r),
    el('h3', { text: 'Summary' }), el('p', { class: 'summary', text: a.summary }),
    SECTIONS.map(([key, label]) => a[key]?.length ? el('section', { class: 'points' },
      el('h3', { text: label }),
      el('ul', {}, a[key].map(item => el('li', {}, el('div', { text: item.text }), el('blockquote', { text: `Employee wrote: "${item.evidence}"` })))),
    ) : null),
    flagsList(a),
    el('p', { class: 'muted small', text: `Produced by ${a.processor}` }),
  );
}

function editor(r, a) {
  const markDirty = () => { state.dirty = true; };
  const wrap = el('div', { class: 'analysis editing', id: 'editor' },
    flagsList(a),
    el('label', { class: 'block' }, el('h3', { text: 'Summary' }),
      el('textarea', { id: 'edit-summary', rows: '5', maxlength: '2000', oninput: markDirty }, a.summary)),
  );
  for (const [key, label] of SECTIONS) {
    const items = a[key] || [];
    const sec = el('section', { class: 'points' }, el('h3', { text: label }));
    if (!items.length) sec.append(el('p', { class: 'muted small', text: 'Nothing found in the employee\'s text.' }));
    const ul = el('ul', {});
    items.forEach((item, i) => {
      const li = el('li', { 'data-section': key, 'data-index': String(i) },
        el('div', { class: 'point-row' },
          el('input', { class: 'point-text', value: item.text, maxlength: '2000', 'aria-label': `${label} point ${i + 1}`, oninput: markDirty }),
          el('button', { class: 'icon', title: 'Remove this point', 'aria-label': 'Remove this point', onclick: () => { li.classList.toggle('removed'); markDirty(); } }, '✕'),
        ),
        el('blockquote', { text: `Employee wrote: "${item.evidence}"` }),
      );
      ul.append(li);
    });
    sec.append(ul); wrap.append(sec);
  }
  wrap.append(el('p', { class: 'muted small', text: `Produced by ${a.processor}. You can reword points; the employee's quote stays attached as evidence.` }));
  return wrap;
}

function collectRevision(r) {
  if (!state.dirty) return null;
  const src = r.extracted;
  const revised = { ...src, summary: $('edit-summary').value.trim(), achievements: [], goals: [], improvements: [], risk_flags: src.risk_flags || [] };
  document.querySelectorAll('#editor li[data-section]').forEach(li => {
    if (li.classList.contains('removed')) return;
    const key = li.dataset.section, i = Number(li.dataset.index);
    const text = li.querySelector('.point-text').value.trim();
    if (text) revised[key].push({ text, evidence: src[key][i].evidence });
  });
  return revised;
}

async function approve(r) {
  let name = null;
  if (!verifiedUser()) {
    name = $('approver').value.trim();
    if (!name) { $('approver').focus(); throw new Error('Enter your name before approving.'); }
    store('rf-approver', name);
  }
  const revised = collectRevision(r);
  const updated = await api(`/api/v1/reviews/${r.id}/approve`, { method: 'PUT', body: JSON.stringify({ revised, approved_by: name }) });
  state.dirty = false;
  toast(revised ? 'Approved with your edits' : 'Approved');
  await select(updated.id, false); await refresh();
}

function openSendBack(r) {
  state.sendBackOpen = true;
  const slot = $('sendback-slot'); slot.replaceChildren();
  const reason = el('textarea', { id: 'sendback-reason', rows: '3', maxlength: '1000', placeholder: 'Tell the employee what to change, e.g. "Please add a measurable target to your goals."' });
  slot.append(el('div', { class: 'sendback' },
    el('label', { class: 'block' }, el('strong', { text: 'Reason for sending back' }), reason),
    el('div', { class: 'actions' },
      el('button', { class: 'danger', onclick: () => act(async () => {
        if (!reason.value.trim()) { reason.focus(); throw new Error('Add a reason so the employee knows what to change.'); }
        const name = $('approver')?.value.trim() || recall('rf-approver') || null;
        await api(`/api/v1/reviews/${r.id}/send-back`, { method: 'POST', body: JSON.stringify({ reason: reason.value.trim(), returned_by: name }) });
        state.sendBackOpen = false; toast('Sent back to the employee');
        await select(r.id, false); await refresh();
      }) }, 'Send back'),
      el('button', { class: 'ghost', onclick: () => { state.sendBackOpen = false; slot.replaceChildren(); } }, 'Cancel'),
    ),
  ));
  reason.focus();
}

async function post(r, action, message) {
  await api(`/api/v1/reviews/${r.id}/${action}`, { method: 'POST' });
  toast(message); await select(r.id, false); await refresh();
}

// ---------- Test submissions ----------
async function simulateTeams() {
  const id = crypto.randomUUID();
  const res = await api('/api/v1/intake/microsoft-forms', { method: 'POST', body: JSON.stringify({
    form_id: 'demo-annual-review-form',
    response_id: `demo-${id}`,
    responder: 'alex.chen@example.com',
    answers: {
      'Employee ID': 'EMP-002',
      'Manager email': 'dana.lee@example.com',
      'Review period': '2026',
      'What were your key achievements this year?': 'Launched the customer self-service portal two weeks early.\nReduced support tickets by 30% after the launch.',
      'What are your goals for next year?': 'Lead the accessibility audit for the web app.',
      'Which areas do you want to develop?': 'Delegating work to newer team members.',
      'Anything else you would like your manager to know?': 'I would like to attend one industry conference.',
    },
  }) });
  toast('Teams form received. The assistant will summarize it.');
  await refresh(); await select(res.id);
}
async function submitForm(form) {
  const body = Object.fromEntries(new FormData(form).entries());
  body.source_system = 'form'; body.source_id = `web-form-${crypto.randomUUID()}`;
  const r = await api('/api/v1/reviews', { method: 'POST', body: JSON.stringify(body) });
  toast('Review submitted. The assistant will summarize it.');
  await refresh(); await select(r.id);
}

// ---------- Wire up ----------
$('signin-form').addEventListener('submit', e => { e.preventDefault(); act(() => signIn($('token').value.trim(), $('remember').checked)); });
$('signout').onclick = signOut;
$('export').onclick = () => act(async () => {
  const blob = await api('/api/v1/workday/staging.csv');
  const url = URL.createObjectURL(blob);
  el('a', { href: url, download: 'reviewflow-workday-staging.csv' }).click();
  setTimeout(() => URL.revokeObjectURL(url), 3000);
  toast('Downloaded reviews that are queued for Workday. This is a generic CSV, not a Workday EIB template.');
});
$('simulate-teams').onclick = () => act(simulateTeams);
$('submit-form').addEventListener('submit', e => { e.preventDefault(); act(() => submitForm(e.currentTarget)); });
window.addEventListener('hashchange', () => { if (state.token) routeFromHash(); });

loadHealth().then(() => {
  if (entra()) { act(startEntra); return; }
  const saved = recall('rf-token');
  if (saved) { $('remember').checked = true; act(() => signIn(saved, true)); }
});
