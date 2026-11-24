// Pure helpers for the herdr-wake Sessions panel. No `herdr` import, so node can test them.
// Adapted in part from inlet/herdr-sessions (MIT, see LICENSE-THIRD-PARTY).

// The agents herdr-wake lists, by the key the app's program table uses for them.
export const AGENTS = { claude: 'Claude Code', codex: 'Codex', pi: 'Pi', opencode: 'OpenCode' };
export const harnessName = s => AGENTS[s.program ?? 'claude'] ?? s.program ?? 'Agent';
// A session id as herdr-wake accepts one in a command: letters, digits, - and _.
export const validId = id => typeof id === 'string' && /^[A-Za-z0-9_-]{1,100}$/.test(id);
// A Herdr space id as `herdr-wake resurrect --space` accepts it.
export const validSpaceId = id => typeof id === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(id);

// One row of `herdr-wake resurrect --json`, as the panel uses it. Fields herdr-wake may not have
// yet (branch, cost, summary and so on) stay undefined, and the panel leaves those facts out.
export function normalizeRow(j) {
  const num = v => (typeof v === 'number' && Number.isFinite(v) ? v : undefined);
  const str = v => (typeof v === 'string' && v ? v : undefined);
  const flags = Array.isArray(j.flags) ? j.flags.filter(f => typeof f === 'string') : [];
  return {
    id: j.session_id,
    program: j.agent && j.agent !== 'claude' ? j.agent : undefined,
    title: str(j.title) ?? '',
    cwd: typeof j.cwd === 'string' ? j.cwd : null,
    exists: j.folder_exists !== false,
    activity: (num(j.last_ms) ?? 0) / 1000,
    ending: str(j.ending),
    branch: str(j.branch),
    flags,
    bypass: j.bypass === true,
    flagsFromTranscript: j.flags_from_transcript === true,
    source: j.source === 'wake' ? 'wake' : 'transcript',
    transcript: str(j.transcript) ?? (j.transcript_gone ? 'gone' : 'unknown'),
    started: num(j.started_ms),
    endedAt: num(j.ended_ms),
    duration: num(j.duration_ms),
    cost: num(j.cost_usd),
    linesAdded: num(j.lines_added),
    linesRemoved: num(j.lines_removed),
    models: Array.isArray(j.models) ? j.models.filter(m => typeof m === 'string') : [],
    prs: Array.isArray(j.prs) ? j.prs.filter(p => p && typeof p.url === 'string' && /^https:\/\//.test(p.url)) : [],
    summary: str(j.summary),
    summarySource: str(j.summary_source),
  };
}

// Why a session cannot be resumed from the panel, or null.
export function cannotResume(s) {
  if (!s.cwd || !s.exists) return 'The project folder no longer exists';
  if (s.transcript === 'gone') return "The agent's session file was deleted";
  if (!validId(s.id)) return 'Invalid session ID';
  return null;
}
// Fork is for Claude Code only.
export const canFork = s => (s.program ?? 'claude') === 'claude' && !cannotResume(s);

// The text of herdr-wake's error, without its "herdr-wake: " lead.
export const cleanError = text => String(text ?? '').split('\n').map(l => l.trim()).filter(Boolean)
  .map(l => l.replace(/^herdr-wake:\s*/, '')).join(' ').slice(0, 400) || 'herdr-wake failed';

// What `herdr-wake resurrect --id X --ticket` printed: where and how to open the terminal.
export function parseTicket(stdout) {
  const t = JSON.parse(stdout);
  if (!Array.isArray(t.argv) || !t.argv.length || !t.argv.every(a => typeof a === 'string')
    || typeof t.cwd !== 'string' || !t.cwd) throw new Error('herdr-wake gave no launch ticket');
  return { argv: t.argv, cwd: t.cwd, title: typeof t.title === 'string' && t.title ? t.title : null };
}

const normalizeFolder = folder => typeof folder === 'string' ? folder.replace(/\/+$/, '') || '/' : '';
// Where a space is now: its first terminal's current folder, not its stale launch folder.
function spaceFolder(space, allPanes) {
  const tabs = new Set((space.tabs ?? []).map(tab => tab.id));
  const live = allPanes.find(pane => tabs.has(pane.tab) && pane.terminal !== null && pane.cwd)?.cwd;
  return normalizeFolder(live || space.folder);
}
// What This space shows, as a scope for filterSessions: the repository the space's folder is in, with
// all its worktrees (from `repos`: folder -> repository); outside a repository that folder with
// everything in it. A space at home or at / covers only that folder itself (`wide`: true tells the
// panel the scope is that broad), and a repository there doesn't count.
export function spaceScope(space, allPanes, repos = {}, home = '') {
  const folder = spaceFolder(space, allPanes);
  if (!folder) return { folder: null };
  const wide = f => f === '/' || f === normalizeFolder(home);
  const repo = normalizeFolder(repos[folder]);
  if (repo && !wide(repo)) return { repo };
  return wide(folder) ? { folder, wide: true } : { under: folder };
}
// The folders of this computer's spaces, to ask git where they are.
export function spaceFolders(allSpaces, allPanes) {
  return allSpaces.filter(space => !space.machine).map(space => spaceFolder(space, allPanes)).filter(Boolean);
}
const placeKey = place => `${place.repo}\u0000${place.tree ?? ''}`;
// Space names by place ("repo\0worktree"), for this computer's spaces whose folder's place is known.
export function spacePlaces(allSpaces, allPanes, places) {
  const names = new Map();
  for (const space of allSpaces) {
    if (space.machine || !space.name) continue;
    const at = places[spaceFolder(space, allPanes)];
    if (at && !names.has(placeKey(at))) names.set(placeKey(at), space.name);
  }
  return names;
}
// A session's place as the sidebar names it: "repo > space" when a space is there and isn't named
// after the repo; otherwise the worktree git or the path gave.
export function labelPlace(place, names) {
  const name = names.get(placeKey(place)), worktree = Boolean(place.tree);
  return name && name !== place.repo ? { repo: place.repo, tree: name, worktree } : { ...place, worktree };
}

// Whether `folder` is `root` or inside it.
function inside(folder, root) {
  const f = normalizeFolder(folder), r = normalizeFolder(root);
  return Boolean(f && r) && (f === r || f.startsWith(r === '/' ? '/' : r + '/'));
}
const base = path => path.replace(/\/+$/, '').split('/').pop();
// Whether a session's folder is in repository `repo`: as git said for that folder (`repos`), or, for a
// folder git wasn't asked about (it's gone), when it's inside the repository or its path names a
// worktree of a repository by that name (~/.herdr/worktrees/<repo>/...).
function inRepo(cwd, repo, repos) {
  const f = normalizeFolder(cwd), r = normalizeFolder(repo);
  if (!f || !r) return false;
  if (Object.hasOwn(repos, cwd)) return normalizeFolder(repos[cwd]) === r;
  if (inside(f, r)) return true;
  const place = placeFromPath(f, '');
  return Boolean(place.tree) && place.repo === base(r).replace(/\.git$/, '');
}
// The sessions in scope `where` whose text has the query. `where`: { folder } is exactly that folder
// (This folder); { repo } every folder of that repository, its worktrees included; { under } that
// folder and everything in it (This space, see spaceScope); null everything (All spaces).
export function filterSessions(rows, query, where, repos = {}) {
  const q = query.trim().toLowerCase();
  const inScope = s => !where || (where.repo !== undefined ? inRepo(s.cwd, where.repo, repos)
    : where.under !== undefined ? inside(s.cwd, where.under)
    : Boolean(normalizeFolder(where.folder)) && normalizeFolder(s.cwd) === normalizeFolder(where.folder));
  return rows.filter(s => inScope(s) && `${s.title} ${s.cwd || ''} ${s.branch || ''} ${s.id} ${harnessName(s)}`.toLowerCase().includes(q));
}

// A word for a shell command: quoted only when it has to be.
export const shellWord = s => /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
// The copy menu's items. The resume command is what `herdr-wake resurrect --id <ID> --print` prints.
export const COPIES = [
  { key: 'session', label: 'Copy session ID', icon: 'files' },
  { key: 'resume', label: 'Copy resume command', icon: 'terminal' },
  { key: 'folder', label: 'Copy folder', icon: 'folder' },
];
export const copyValues = (s, resumeCommand) => ({ session: s.id ?? null, resume: resumeCommand || null, folder: s.cwd || null });

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
export function formatSessionTime(milliseconds) {
  if (typeof milliseconds !== 'number' || !Number.isFinite(milliseconds) || milliseconds <= 0) return null;
  const d = new Date(milliseconds);
  if (!Number.isFinite(d.getTime())) return null;
  const pad = n => String(n).padStart(2, '0');
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}, ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
// How long ago, in short: now, 50m, 3h, 2d, 4mo, 1y. `seconds`: epoch seconds.
export function ago(seconds, now = Date.now()) {
  const s = now / 1000 - seconds;
  for (const [n, u] of [[31536000, 'y'], [2592000, 'mo'], [86400, 'd'], [3600, 'h'], [60, 'm']]) if (s >= n) return `${Math.floor(s / n)}${u}`;
  return 'now';
}
export function compactSummary(text) {
  if (typeof text !== 'string' || !text.trim()) return null;
  const words = text.trim().split(/\s+/).filter(Boolean);
  return words.length > 60 ? words.slice(0, 60).join(' ') + '…' : words.join(' ');
}
// Where a session ran, named as the sidebar names a space: "repo > worktree".
// From `git rev-parse --path-format=absolute --git-common-dir --show-toplevel` run in its folder.
export function placeFromGit(stdout, cwd) {
  const top = (stdout.split('\n')[1] ?? '').trim(), root = repoFromGit(stdout) ?? '';
  const repo = (root ? base(root) : base(cwd)).replace(/\.git$/, '');
  return { repo, tree: top && root && top !== root ? base(top) : null };
}
// The repository a folder is in, from the same git output: the common folder without its /.git (a
// worktree's is its main checkout's), else that folder itself (a bare repository); null without one.
export function repoFromGit(stdout) {
  const common = (stdout.split('\n')[0] ?? '').trim();
  return !common ? null : common.endsWith('/.git') ? common.slice(0, -5) : common;
}
// Where worktree tools put their checkouts; the folders tell repo and worktree even after
// git forgot them.
const WORKTREES = [
  /\/\.herdr\/worktrees\/(?<repo>[^/]+)\/(?<tree>[^/]+)/,
  /\/orca\/workspaces\/(?<repo>[^/]+)\/(?<tree>[^/]+)/,
  /\/(?<repo>[^/]+)\/\.claude\/worktrees\/(?<tree>[^/]+)/,
];
// Without git (the folder is gone, or no repository): a worktree folder above still tells both;
// home is ~; anything else is its folder's name.
export function placeFromPath(cwd, home) {
  if (typeof cwd !== 'string' || !cwd.replace(/\/+$/, '')) return { repo: 'Unknown folder', tree: null };
  if (home && cwd.replace(/\/+$/, '') === home.replace(/\/+$/, '')) return { repo: '~', tree: null };
  for (const pattern of WORKTREES) {
    const m = pattern.exec(cwd);
    if (m) return { repo: m.groups.repo, tree: m.groups.tree };
  }
  return { repo: base(cwd), tree: null };
}
export function formatDuration(ms) {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return null;
  const minutes = Math.round(ms / 60000);
  if (minutes < 1) return `${Math.max(1, Math.round(ms / 1000))}s`;
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}
export function formatCost(usd) {
  if (typeof usd !== 'number' || !Number.isFinite(usd) || usd < 0) return null;
  return usd < 0.01 ? '<$0.01' : `$${usd.toFixed(2)}`;
}
// What a row's dot says. `ending`: finished, unanswered, interrupted, mid-action (read from the
// transcript), or quit, killed, replaced, forgotten, unknown, open (what the journal saw).
export function sessionState(s) {
  if (!s.exists) return { key: 'missing', label: "Folder missing, can't be resumed" };
  if (s.transcript === 'gone') return { key: 'missing', label: "Session file deleted, can't be resumed" };
  switch (s.ending) {
    case 'unanswered': return { key: 'stopped', label: 'Stopped mid-work: your last message got no answer' };
    case 'interrupted': return { key: 'stopped', label: 'Stopped mid-work: interrupted' };
    case 'mid-action': return { key: 'stopped', label: 'Stopped mid-work: during an action' };
    case 'finished': return { key: 'finished', label: 'Finished' };
    case 'quit': return { key: 'closed', label: 'Closed: you quit the agent' };
    case 'killed': return { key: 'closed', label: 'Closed: the agent was killed' };
    case 'replaced': return { key: 'closed', label: 'Closed: replaced by a new session in its terminal' };
    case 'forgotten': return { key: 'closed', label: 'Closed: forgotten by herdr-wake' };
    default: return { key: 'empty', label: 'Ending not recorded' };
  }
}
// The dot's color for a state, from the theme.
export function stateColor(key, t) {
  return { finished: t.ansi2, stopped: t.ansi3, missing: t.ansi1 }[key] ?? t.dim;
}
// claude-opus-5-5 -> Opus 5.5; anything else as it is.
export function modelName(id) {
  const m = /^claude-([a-z]+)-(\d+)-(\d+)\b/.exec(id ?? '');
  return m ? `${m[1][0].toUpperCase()}${m[1].slice(1)} ${m[2]}.${m[3]}` : id;
}
