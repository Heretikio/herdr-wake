// Run: node test_logic.mjs
import assert from 'node:assert/strict';
import * as L from './logic.mjs';

const wire = {
  agent: 'claude', session_id: '51ca3726-ea6a-40a0-834f-cf76a683d672', cwd: '/Users/me', title: 'Itch', ending: 'unknown',
  last_ms: 1795553123554, flags: ['--permission-mode', 'auto'], bypass: true, flags_from_transcript: true, source: 'transcript',
  transcript_gone: false, transcript: 'present', folder_exists: true, ended_ms: null,
};
// A row from today's JSON: later fields stay undefined.
const r = L.normalizeRow(wire);
assert.equal(r.id, wire.session_id);
assert.equal(r.program, undefined);
assert.equal(r.activity, 1795553123.554);
assert.equal(r.branch, undefined);
assert.deepEqual(r.models, []);
assert.deepEqual(r.prs, []);
assert.equal(r.bypass, true);
assert.equal(r.exists, true);
assert.equal(L.normalizeRow({ ...wire, agent: 'codex' }).program, 'codex');
// A row with the fields herdr-wake may add.
const full = L.normalizeRow({ ...wire, branch: 'main', cost_usd: 2.413, duration_ms: 125000, started_ms: 1, lines_added: 3,
  lines_removed: 1, models: ['claude-opus-5-5'], prs: [{ number: 1, url: 'https://x/1', repo: 'a/b' }, { number: 2, url: 'javascript:x' }],
  summary: 'Did a thing', summary_source: 'recap', ending: 'unanswered' });
assert.equal(full.prs.length, 1);
assert.equal(full.summarySource, 'recap');
assert.equal(L.sessionState(full).key, 'stopped');
assert.equal(L.formatCost(full.cost), '$2.41');
assert.equal(L.formatDuration(full.duration), '2m');
assert.equal(L.modelName('claude-opus-5-5'), 'Opus 5.5');
// States.
assert.equal(L.sessionState({ exists: false }).key, 'missing');
assert.equal(L.sessionState({ exists: true, transcript: 'gone' }).key, 'missing');
assert.equal(L.sessionState({ exists: true, ending: 'finished' }).key, 'finished');
assert.equal(L.sessionState({ exists: true, ending: 'quit' }).key, 'closed');
assert.equal(L.sessionState({ exists: true, ending: 'unknown' }).key, 'empty');
// What can be resumed.
assert.equal(L.cannotResume(r), null);
assert.match(L.cannotResume({ ...r, exists: false }), /folder/);
assert.match(L.cannotResume({ ...r, transcript: 'gone' }), /deleted/);
assert.match(L.cannotResume({ ...r, id: 'a b' }), /Invalid/);
assert.equal(L.canFork(r), true);
assert.equal(L.canFork({ ...r, program: 'codex' }), false);
assert.equal(L.validSpaceId('sp7ca99d'), true);
assert.equal(L.validSpaceId('a b'), false);
assert.equal(L.validSpaceId('x'.repeat(65)), false);
// herdr-wake's words.
assert.equal(L.cleanError('herdr-wake: the folder is not safe\n'), 'the folder is not safe');
assert.deepEqual(L.parseTicket('{"key":"k","argv":["/b/hw","resurrect","--run","k"],"cwd":"/w","title":"T"}'),
  { argv: ['/b/hw', 'resurrect', '--run', 'k'], cwd: '/w', title: 'T' });
assert.throws(() => L.parseTicket('{"argv":[],"cwd":"/w"}'));
assert.throws(() => L.parseTicket('{"argv":["a"],"cwd":""}'));
assert.equal(L.parseTicket('{"argv":["a"],"cwd":"/w","title":null}').title, null);
// Scope.
const at = cwd => ({ ...r, cwd });
const shown = where => L.filterSessions([at('/repo'), at('/repo/app'), at('/repository'), at('/other'), at(null)], '', where).map(x => x.cwd);
assert.deepEqual(shown({ under: '/repo' }), ['/repo', '/repo/app']);
assert.deepEqual(shown({ folder: '/repo' }), ['/repo']);
assert.deepEqual(shown(null).length, 5);
assert.deepEqual(L.filterSessions([r], 'itch', null).length, 1);
assert.deepEqual(L.filterSessions([r], 'codex', null).length, 0);
const repos = { '/code/shop': '/code/shop', '/w/shop/feat': '/code/shop' };
assert.deepEqual(L.filterSessions(['/code/shop', '/w/shop/feat', '/other'].map(at), '', { repo: '/code/shop' }, repos).map(x => x.cwd),
  ['/code/shop', '/w/shop/feat']);
const space = { id: 'sp1', machine: null, name: 'webshop', folder: '/Users/me', tabs: [] };
assert.deepEqual(L.spaceScope(space, [], {}, '/Users/me'), { folder: '/Users/me', wide: true });
assert.deepEqual(L.spaceScope({ ...space, folder: '/Users/me/p' }, [], {}, '/Users/me'), { under: '/Users/me/p' });
assert.deepEqual(L.spaceScope({ ...space, folder: '/Users/me/p' }, [], { '/Users/me/p': '/Users/me/p' }, '/Users/me'), { repo: '/Users/me/p' });
// Copy values.
assert.deepEqual(L.copyValues(r, 'cd /x && claude'), { session: r.id, resume: 'cd /x && claude', folder: '/Users/me' });
assert.equal(L.copyValues(r, null).resume, null);
// Places and ages.
assert.deepEqual(L.placeFromPath('/Users/me', '/Users/me'), { repo: '~', tree: null });
assert.deepEqual(L.placeFromPath('/Users/me/.herdr/worktrees/shop/feat', '/Users/me'), { repo: 'shop', tree: 'feat' });
assert.equal(L.ago(1000, 1000 * 1000 + 3 * 3600 * 1000), '3h');
assert.equal(L.compactSummary('a b c'), 'a b c');
assert.equal(L.compactSummary('w '.repeat(100)).split(' ').length, 60);
assert.equal(L.compactSummary('  '), null);


// ---- Machines ----
// Names: what `hc exec --on` may be given.
for (const ok of ['box1', 'laptop', 'a.b-c_d', 'x'.repeat(64)]) assert.equal(L.validMachine(ok), true, ok);
for (const bad of ['', '-x', '--on', '.hidden', 'a b', 'a/b', 'a;b', '$(x)', "a'b", 'a\nb', 'x'.repeat(65), null, 7]) assert.equal(L.validMachine(bad), false, String(bad));
// `hc machine ls --json`: connected or not, no temporary or invalid names, no repeats.
const ls = JSON.stringify({ machines: [
  { name: 'box1', state: 'connected', temporary: false }, { name: 'laptop', state: 'disconnected' },
  { name: 'sandbox', state: 'connected', temporary: true }, { name: '--evil', state: 'connected' }, { name: 'box1', state: 'connected' }] });
assert.deepEqual(L.parseMachines(ls), [{ name: 'box1', connected: true }, { name: 'laptop', connected: false }]);
assert.deepEqual(L.parseMachines('not json'), []);
assert.deepEqual(L.parseMachines('{}'), []);
// The command: an argv, the script fixed, everything else after it as arguments.
const argv = L.remoteArgs('box1', ['resurrect', '--id', "x'; rm -rf ~"], 20);
assert.deepEqual(argv.slice(0, 7), ['exec', '--on', 'box1', '--timeout', '20', '--', 'sh']);
assert.equal(argv[8], L.WAKE_SCRIPT);
assert.deepEqual(argv.slice(9), ['herdr-wake', 'resurrect', '--id', "x'; rm -rf ~"]);
assert.match(L.WAKE_SCRIPT, /\$HOME\/\.local\/bin\/herdr-wake.*command -v herdr-wake/);
assert.ok(!L.WAKE_SCRIPT.includes('resurrect'));
assert.throws(() => L.remoteArgs('a b', []), /machine/);
assert.throws(() => L.remoteArgs('-x', []), /machine/);
assert.throws(() => L.remoteArgs('box1', [1]), /arguments/);
assert.equal(L.remoteArgs('box1', [], 9999)[4], '600');
// Space ids: another machine's shown as "machine/id"; the label on that machine has none.
assert.equal(L.localSpaceId('box1/sp1', 'box1'), 'sp1');
assert.equal(L.localSpaceId('sp1', null), 'sp1');
assert.equal(L.validSpaceId(L.localSpaceId('box1/sp1', 'box1')), true);
assert.equal(L.validSpaceId('box1/sp1'), false);
// Where Resume opens: the focused space on that machine, else the first one there, else none.
const sps = [{ id: 'sp1', machine: null }, { id: 'box1/spa', machine: 'box1' }, { id: 'box1/spb', machine: 'box1' }, { id: 'laptop/spc', machine: 'laptop' }];
assert.equal(L.resumeSpace('box1', sps[2], sps).id, 'box1/spb');
assert.equal(L.resumeSpace('box1', sps[0], sps).id, 'box1/spa');
assert.equal(L.resumeSpace(null, sps[1], sps).id, 'sp1');
assert.equal(L.resumeSpace(null, sps[0], sps).id, 'sp1');
assert.equal(L.resumeSpace('nas', sps[0], sps), null);
// A row's machine, and its key.
const rb = L.normalizeRow(wire, 'box1');
assert.equal(rb.machine, 'box1');
assert.equal(r.machine, null);
assert.equal(L.rowKey(rb), 'box1/' + wire.session_id);
assert.equal(L.rowKey(r), wire.session_id);
// Place line: the machine first for a row from another machine, nothing extra here.
const atHome = L.placeFromPath('/home/x', '/home/x');
assert.equal(L.placeText(L.labelPlace(atHome, new Map(), 'box1')), 'box1 · ~');
assert.equal(L.placeText(L.labelPlace({ repo: 'shop', tree: 'feat' }, new Map(), 'box1')), 'box1 · shop › feat');
assert.equal(L.placeText(L.labelPlace({ repo: 'shop', tree: null }, new Map([['shop\u0000', 'main']]))), 'shop › main');
assert.equal(L.placeText(L.labelPlace(atHome, new Map())), '~');
assert.equal(L.guessHome(['/home/x/a', '/home/x', '/home/y/z', '/home/x/b']), '/home/x');
assert.equal(L.guessHome(['/Users/me/p', '/srv/app']), '/Users/me');
assert.equal(L.guessHome(['/srv/app', null]), '');
// Machine filter, and the filter text matching the machine's name.
const mixed = [wire, { ...wire, session_id: 'b1', title: 'Deploy' }].map((j, i) => L.normalizeRow(j, i ? 'box1' : null));
const names = rows => rows.map(x => x.machine ?? 'here');
assert.deepEqual(names(L.filterSessions(mixed, '', null, {}, L.ALL_MACHINES)), ['here', 'box1']);
assert.deepEqual(names(L.filterSessions(mixed, '', null, {})), ['here', 'box1']);
assert.deepEqual(names(L.filterSessions(mixed, '', null, {}, L.THIS_COMPUTER)), ['here']);
assert.deepEqual(names(L.filterSessions(mixed, '', null, {}, 'box1')), ['box1']);
assert.deepEqual(names(L.filterSessions(mixed, '', null, {}, 'laptop')), []);
assert.deepEqual(names(L.filterSessions(mixed, 'BOX1', null, {})), ['box1']);
assert.deepEqual(names(L.filterSessions(mixed, 'deploy', null, {}, 'box1')), ['box1']);
// This folder / This space follow the machine of the focused space: its folders, its machine's rows only.
const same = mixed.map(x => ({ ...x, cwd: '/proj' }));
assert.deepEqual(names(L.filterSessions(same, '', { under: '/proj', machine: null }, {})), ['here']);
assert.deepEqual(names(L.filterSessions(same, '', { under: '/proj', machine: 'box1' }, {})), ['box1']);
assert.deepEqual(names(L.filterSessions(same, '', { folder: '/proj', machine: 'box1' }, {})), ['box1']);
assert.deepEqual(names(L.filterSessions(same, '', { folder: null, machine: 'box1' }, {})), []);
assert.deepEqual(names(L.filterSessions(same, '', { under: '/proj', machine: 'box1' }, {}, L.THIS_COMPUTER)), []);
assert.deepEqual(names(L.filterSessions(same, '', { under: '/proj' }, {})), ['here', 'box1']);
// The picker's choices; a machine that stopped answering falls back to all.
assert.deepEqual(L.machineOptions(['laptop', 'box1']).map(o => o.key), ['*', '', 'box1', 'laptop']);
assert.equal(L.machineOptions(['box1'], new Map([['box1', 'offline']]))[2].label, 'box1 (offline)');
assert.equal(L.headline({ title: 'fix it', summary: 'Fixed the panel row layout. Then more.' }), 'Fixed the panel row layout.');
assert.equal(L.headline({ title: 'fix it', summary: 'Short. Then the rest of it' }), 'Short. Then the rest of it');
assert.equal(L.headline({ title: 'fix it' }), 'fix it');
assert.equal(L.headline({ title: '', program: 'codex' }), 'Codex session');
assert.equal(L.effectivePick('box1', ['box1']), 'box1');
assert.equal(L.effectivePick('box1', []), '*');
assert.equal(L.effectivePick('', []), '');
assert.equal(L.effectivePick(undefined, ['box1']), '*');
// Merging: newest first across machines; the same session id on two machines stays two rows.
const older = L.normalizeRow({ ...wire, last_ms: 1000 }, 'box1');
const newer = L.normalizeRow({ ...wire, last_ms: 2000 }, 'laptop');
const merged = L.mergeRows(new Map([[null, [r]], ['box1', [older]], ['laptop', [newer]]]));
assert.deepEqual(merged.map(x => x.machine), [null, 'laptop', 'box1']);
assert.equal(new Set(merged.map(L.rowKey)).size, 3);
assert.deepEqual(L.mergeRows(new Map()), []);
// What a remote call came to.
const hcErr = kind => JSON.stringify({ error: { kind, message: 'x' } });
assert.deepEqual(L.classifyRemote({ code: 0, stdout: '{"sessions":[]}', stderr: '' }), { state: 'ok', sessions: [] });
assert.equal(L.classifyRemote({ code: 127, stdout: '', stderr: 'herdr-wake: not installed\n' }).state, 'missing');
assert.equal(L.classifyRemote({ code: 1, stdout: '', stderr: hcErr('spawn_failed') }).state, 'missing');
assert.equal(L.classifyRemote({ code: 2, stdout: '', stderr: "error: unrecognized subcommand 'resurrect'\n\nUsage: herdr-wake <COMMAND>" }).state, 'old');
assert.equal(L.classifyRemote({ code: 4, stdout: '', stderr: hcErr('machine_unknown') }).state, 'unreachable');
assert.equal(L.classifyRemote({ code: 1, stdout: '', stderr: hcErr('timeout') }).state, 'slow');
assert.equal(L.classifyRemote({ code: 124, stdout: '', stderr: '' }).state, 'slow');
assert.deepEqual(L.classifyRemote({ code: 1, stdout: '', stderr: 'herdr-wake: it broke\n' }), { state: 'error', text: 'it broke' });
assert.equal(L.classifyRemote({ code: 0, stdout: 'nope', stderr: '' }).state, 'error');
// The quiet notes: one line per distinct text, machines together.
assert.equal(L.machineNote({ state: 'old' }), 'herdr-wake with resurrect not installed');
assert.equal(L.machineNote({ state: 'offline' }), 'offline');
assert.equal(L.machineNote({ state: 'ok' }), null);
assert.deepEqual(L.noteLines([{ machine: 'box1', text: 'offline' }, { machine: 'laptop', text: 'herdr-wake with resurrect not installed' },
  { machine: 'nas', text: 'offline' }]), ['box1, nas: offline', 'laptop: herdr-wake with resurrect not installed']);
assert.deepEqual(L.noteLines([]), []);
// Results for the callers that read { code, stdout, stderr }.
assert.deepEqual(L.remoteResult('box1', { code: 0, stdout: 'x', stderr: '' }), { code: 0, stdout: 'x', stderr: '' });
assert.deepEqual(L.remoteResult('box1', { code: 3, stdout: '', stderr: 'herdr-wake: no\n' }), { code: 3, stdout: '', stderr: 'herdr-wake: no\n' });
assert.equal(L.remoteResult('box1', { code: 1, stdout: '', stderr: hcErr('machine_offline') }).stderr, 'box1 is not reachable');
assert.equal(L.remoteResult('box1', { code: 1, stdout: '', stderr: hcErr('spawn_failed') }).code, 127);
// Prefetch: 20 here, 5 on another machine.
assert.equal(L.prefetchCount(null), 60);
assert.equal(L.prefetchCount('box1'), 20);

// One machine through a fake `hc`: it parses, tags rows, and never rejects or waits for a slow one.
const sessionsJson = id => JSON.stringify({ sessions: [{ ...wire, session_id: id }] });
const calls = [];
const fakeHc = async args => {
  calls.push(args);
  const machine = args[2];
  if (machine === 'box1') return { code: 0, stdout: sessionsJson('s1'), stderr: '' };
  if (machine === 'laptop') return { code: 2, stdout: '', stderr: "error: unrecognized subcommand 'resurrect'" };
  if (machine === 'nas') throw new Error('hc is gone');
  return new Promise(() => {}); // never answers
};
const t0 = Date.now();
const done = await Promise.all(['box1', 'laptop', 'nas', 'slowbox'].map(m => L.loadMachine(m, fakeHc, 60)));
assert.ok(Date.now() - t0 < 1000);
assert.deepEqual(done.map(x => x.state), ['ok', 'old', 'unreachable', 'slow']);
assert.equal(done[0].rows[0].machine, 'box1');
assert.equal(done[0].rows[0].id, 's1');
assert.deepEqual(calls[0].slice(0, 6), ['exec', '--on', 'box1', '--timeout', '6', '--']);
assert.deepEqual(calls[0].slice(-4), ['resurrect', '--json', '--limit', '0']);
assert.equal((await L.loadMachine('bad name', fakeHc, 60)).state, 'unreachable');
console.log('ok');
