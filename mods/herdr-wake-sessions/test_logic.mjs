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
console.log('ok');
