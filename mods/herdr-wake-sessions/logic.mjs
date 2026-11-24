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
export function normalizeRow(j, machine = null) {
  const num = v => (typeof v === 'number' && Number.isFinite(v) ? v : undefined);
  const str = v => (typeof v === 'string' && v ? v : undefined);
  const flags = Array.isArray(j.flags) ? j.flags.filter(f => typeof f === 'string') : [];
  return {
    id: j.session_id,
    // The Herdr machine the session is on (`hc machine ls`); null is this computer.
    machine: machine || null,
    program: j.agent && j.agent !== 'claude' ? j.agent : undefined,
    title: str(j.title) ?? '',
    cwd: typeof j.cwd === 'string' ? j.cwd : null,
    exists: j.folder_exists !== false,
    activity: (num(j.last_ms) ?? 0) / 1000,
    // How the conversation stood (finished, stopped mid-work) says more than how the journal saw
    // the agent end (quit, killed), so it wins when herdr-wake read it.
    ending: str(j.conversation) ?? str(j.ending),
    closedBy: str(j.ending),
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
// `machine`: a row from another machine carries its name, which the place line shows first.
export function labelPlace(place, names, machine = null) {
  const name = names.get(placeKey(place)), worktree = Boolean(place.tree);
  const label = name && name !== place.repo ? { repo: place.repo, tree: name, worktree } : { ...place, worktree };
  return machine ? { ...label, machine } : label;
}
// The place line as text (the copy menu's title, the tests): "laptop · ~ › repo", the machine only
// for a row that is not from this computer.
export const placeText = ({ machine, repo, tree }) => `${machine ? machine + ' · ' : ''}${repo}${tree ? ` › ${tree}` : ''}`;

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
// Whether a row is on the machine the picker chose: ALL_MACHINES, THIS_COMPUTER or a machine name.
export const machineMatches = (s, pick = ALL_MACHINES) => pick === ALL_MACHINES || (s.machine ?? '') === pick;
// The sessions in scope `where` whose text has the query, on the machine `pick` chose. `where`:
// { folder } is exactly that folder (This folder); { repo } every folder of that repository, its
// worktrees included; { under } that folder and everything in it (This space, see spaceScope); null
// everything (All spaces). `where.machine` (a name, or null for this computer) limits it to the
// machine of the focused space; the folders are that machine's. The query also matches the machine name.
export function filterSessions(rows, query, where, repos = {}, pick = ALL_MACHINES) {
  const q = query.trim().toLowerCase();
  const inScope = s => !where || (where.machine !== undefined && (s.machine ?? null) !== where.machine ? false
    : where.repo !== undefined ? inRepo(s.cwd, where.repo, repos)
    : where.under !== undefined ? inside(s.cwd, where.under)
    : Boolean(normalizeFolder(where.folder)) && normalizeFolder(s.cwd) === normalizeFolder(where.folder));
  return rows.filter(s => machineMatches(s, pick) && inScope(s)
    && `${s.title} ${s.cwd || ''} ${s.branch || ''} ${s.id} ${harnessName(s)} ${s.machine || ''}`.toLowerCase().includes(q));
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

// ---- Machines: this computer and the others `hc machine ls` lists ----

// The picker's values: every machine, this computer, or a machine's name (a valid name is never
// empty or "*").
export const ALL_MACHINES = '*';
export const THIS_COMPUTER = '';
// A machine name as the panel passes it to `hc exec --on`: letters, digits, - _ and ., starting with
// a letter or digit (so it is never read as an option), at most 64 characters.
export const validMachine = name => typeof name === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name);
// `hc machine ls --json` as [{ name, connected }]: no temporary machines, no invalid or repeated names.
export function parseMachines(stdout) {
  let list;
  try { list = JSON.parse(stdout)?.machines; } catch (_e) { return []; }
  if (!Array.isArray(list)) return [];
  const seen = new Set(), out = [];
  for (const m of list) {
    if (!m || m.temporary === true || !validMachine(m.name) || seen.has(m.name)) continue;
    seen.add(m.name);
    out.push({ name: m.name, connected: m.state === 'connected' });
  }
  return out;
}
// The space id `herdr-wake resurrect --space` takes on that machine: Herdr shows another machine's
// space ids as "<machine>/<id>", and the labels on that machine have no prefix.
export const localSpaceId = (id, machine) => machine && typeof id === 'string' && id.startsWith(machine + '/') ? id.slice(machine.length + 1) : id;
// The space a session opens in: the focused one when it is on the session's machine, else the first
// space there; null when that machine has none (a remote one is then created). `machine`: null here.
export function resumeSpace(machine, focusedSpace, allSpaces) {
  const on = sp => (sp.machine ?? null) === (machine ?? null);
  if (focusedSpace && on(focusedSpace)) return focusedSpace;
  return allSpaces.find(on) ?? null;
}
// A sh script that runs the remote herdr-wake: ~/.local/bin first (the remote core's PATH may not
// have it), then PATH. It is fixed text; everything that varies is an argument ("$@"), never part of
// the script, and `hc exec` runs the argv with no shell around it.
export const WAKE_SCRIPT = 'b="$HOME/.local/bin/herdr-wake"; [ -x "$b" ] || b=$(command -v herdr-wake) '
  + '|| { echo "herdr-wake: not installed" >&2; exit 127; }; exec "$b" "$@"';
// The `hc` arguments that run `herdr-wake <wakeArgs>` on `machine`, ended after `seconds`.
export function remoteArgs(machine, wakeArgs, seconds = 60) {
  if (!validMachine(machine)) throw new Error('invalid machine name');
  if (!Array.isArray(wakeArgs) || !wakeArgs.every(a => typeof a === 'string')) throw new Error('invalid arguments');
  const secs = Math.min(600, Math.max(1, Math.ceil(Number(seconds) || 60)));
  return ['exec', '--on', machine, '--timeout', String(secs), '--', 'sh', '-c', WAKE_SCRIPT, 'herdr-wake', ...wakeArgs];
}
// What `hc` itself reports (a machine it cannot reach, a program it cannot start) is JSON on stderr:
// {"error":{"kind":...,"message":...}}. null when stderr is something else (the program's own).
export function hcError(stderr) {
  const line = String(stderr ?? '').trim().split('\n').filter(Boolean).pop() ?? '';
  try {
    const e = JSON.parse(line)?.error;
    return e && typeof e === 'object' && typeof e.kind === 'string' ? e : null;
  } catch (_e) { return null; }
}
// What one remote `herdr-wake resurrect --json` call came to: { state: 'ok', sessions } or a state
// that says why there are none: missing (no herdr-wake), old (no `resurrect`), slow, offline,
// unreachable, error ({ text }).
export function classifyRemote(r) {
  const err = hcError(r.stderr);
  if (err) {
    if (err.kind === 'spawn_failed') return { state: 'missing' };
    if (/timeout|timed_out/.test(err.kind)) return { state: 'slow' };
    if (/offline|disconnected/.test(err.kind)) return { state: 'offline' };
    return { state: 'unreachable' };
  }
  if (r.code === 127) return { state: 'missing' };
  if (r.code === 124) return { state: 'slow' };
  if (r.code === 2 && /unrecognized subcommand|unexpected argument|unrecognized/i.test(r.stderr ?? '')) return { state: 'old' };
  if (r.code !== 0) return { state: 'error', text: cleanError(r.stderr) };
  try {
    const list = JSON.parse(r.stdout)?.sessions;
    if (Array.isArray(list)) return { state: 'ok', sessions: list };
  } catch (_e) { /* not JSON */ }
  return { state: 'error', text: 'herdr-wake gave no session list' };
}
// A remote call's result as the callers that talk to herdr-wake read it ({ code, stdout, stderr }):
// what `hc` itself reported becomes a plain sentence on stderr. A program that is not there gives 127.
export function remoteResult(machine, r) {
  const err = hcError(r.stderr);
  if (!err) return { code: r.code, stdout: r.stdout ?? '', stderr: r.stderr ?? '' };
  const text = err.kind === 'spawn_failed' ? `herdr-wake is not installed on ${machine}`
    : /timeout|timed_out/.test(err.kind) ? `${machine} did not answer in time` : `${machine} is not reachable`;
  return { code: err.kind === 'spawn_failed' ? 127 : 1, stdout: '', stderr: text };
}
const NOTES = { missing: 'herdr-wake with resurrect not installed', old: 'herdr-wake with resurrect not installed',
  slow: 'no answer in 15 s', offline: 'offline', unreachable: 'not reachable' };
// The quiet footer text for a machine that gave no list, or null (it answered).
export const machineNote = c => c.state === 'ok' ? null : (c.state === 'error' ? c.text : NOTES[c.state]) ?? null;
// Footer lines: machines with the same note share one ("box1, laptop: herdr-wake with resurrect not
// installed"), in the order the machines came.
export function noteLines(notes) {
  const groups = new Map();
  for (const { machine, text } of notes) if (text) groups.set(text, [...(groups.get(text) ?? []), machine]);
  return [...groups].map(([text, names]) => `${names.join(', ')}: ${text}`);
}
// A promise that gives up after `ms`: resolves with `onTimeout` instead. `after(ms, callback)` sets the
// timer (the Herdr shell has no setTimeout: the panel passes cx.timer.after; node's is the default). A
// timer that is no longer needed fires and changes nothing.
export function withTimeout(promise, ms, onTimeout, after = (n, f) => setTimeout(f, n)) {
  return Promise.race([promise, new Promise(resolve => { after(ms, () => resolve(onTimeout)); })]);
}
// One machine's sessions through `runHc(args)` (the `hc` call), within `ms` and never rejecting:
// { state, rows } with the rows as normalizeRow reads them, tagged with the machine.
export async function loadMachine(machine, runHc, ms = 15000, after = undefined) {
  let r;
  try {
    const args = remoteArgs(machine, ['resurrect', '--json', '--limit', '0'], Math.ceil(ms / 1000) + 5);
    r = await withTimeout(runHc(args), ms, { code: 124, stdout: '', stderr: '' }, after);
  } catch (e) { r = { code: 1, stdout: '', stderr: JSON.stringify({ error: { kind: 'unreachable', message: String(e?.message ?? e) } }) }; }
  const c = classifyRemote(r);
  if (c.state !== 'ok') return c;
  const rows = c.sessions.map(j => normalizeRow(j, machine)).filter(s => typeof s.id === 'string');
  return { state: 'ok', rows };
}
// The rows of every machine in one list, newest first. `byMachine`: Map of machine (null: this
// computer) to its rows.
export function mergeRows(byMachine) {
  const all = [];
  for (const rows of byMachine.values()) all.push(...rows);
  return all.sort((a, b) => b.activity - a.activity);
}
// A row's key in the list and in the launched set: the id, with the machine when it is not here (the
// same id can be on two machines).
export const rowKey = s => s.machine ? `${s.machine}/${s.id}` : s.id;
// The picker's choices: all, this computer, and each machine that answered.
export function machineOptions(answered) {
  return [{ key: ALL_MACHINES, label: 'All machines' }, { key: THIS_COMPUTER, label: 'This computer' },
    ...[...answered].sort().map(name => ({ key: name, label: name }))];
}
// The picked machine if it is still a choice, else all.
export const effectivePick = (pick, answered) =>
  pick === THIS_COMPUTER || (typeof pick === 'string' && answered.includes(pick)) ? pick : ALL_MACHINES;
// The home folder of a machine, guessed from where its sessions ran (/Users/x, /home/x, /root), so a
// session at home reads "~". '' when nothing says.
export function guessHome(cwds) {
  const count = new Map();
  for (const c of cwds) {
    const m = typeof c === 'string' ? /^(\/Users\/[^/]+|\/home\/[^/]+|\/root)(?=\/|$)/.exec(c) : null;
    if (m) count.set(m[1], (count.get(m[1]) ?? 0) + 1);
  }
  return [...count].sort((a, b) => b[1] - a[1])[0]?.[0] ?? '';
}
// How many of a machine's newest sessions get a background summary: all 20 here, 5 on a machine
// that is not (each one costs that machine's Claude usage).
export const prefetchCount = machine => machine ? 5 : 20;

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
