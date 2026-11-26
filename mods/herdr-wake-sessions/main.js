// The herdr-wake Sessions panel: closed agent sessions, from `herdr-wake resurrect`, with Resume.
// Every fact comes from the herdr-wake program through `exec`; this file reads no transcript.
// The look and the list mechanics are adapted from inlet/herdr-sessions (MIT, see LICENSE-THIRD-PARTY).
import { View, div, image, svg } from 'gpui-kit';
import { v_flex, h_flex, InputState, Input, Scrollbar, VirtualListScrollHandle, v_virtual_list } from 'gpui-base';
import { exec, hc, home, checking, theme, run, focused, spaces, panes, layout, knownAgents, settings,
  props, openOverlay, closeOverlay, notify, shared, setShared } from 'herdr';
import { normalizeRow, filterSessions, spaceScope, repoFromGit, ago, sessionState, placeFromGit, placeFromPath,
  spaceFolders, spacePlaces, labelPlace, COPIES, copyValues, cannotResume, canFork, cleanError, parseTicket,
  validSpaceId, validId, harnessName, ALL_MACHINES, THIS_COMPUTER, parseMachines, loadMachine, remoteArgs, remoteResult,
  withTimeout, mergeRows, rowKey, machineOptions, effectivePick, machineNote, noteLines, guessHome, prefetchCount,
  placeText, localSpaceId, resumeSpace, headline } from './logic.mjs';
import { sessionDetail } from './details.js';
import { segmented, iconButton, placeLine, pickerButton } from './ui.js';

const MOD = 'herdr-wake-sessions';
const PANEL = 'herdr-wake-sessions-panel';
const ROW = 46;
const PICK = 'herdr-wake-sessions.machine'; // the machine picker's choice, shared with its menu
const REMOTE_EVERY = 60000; // other machines are asked again this often while the panel is shown
const SCOPES = [['folder', 'This folder'], ['space', 'This space'], ['all', 'All spaces']];

// A timer for withTimeout from a task's cx (the shell has no setTimeout).
const afterOf = cx => (ms, callback) => { cx.timer.after(ms, () => callback()); };

// The nodes from the layout's root down to this mod's panel block.
function panelPath(node, path = []) {
  if (!node) return null;
  path = [...path, node];
  if (node.id === PANEL) return path;
  for (const child of node.children ?? []) {
    const found = panelPath(child, path);
    if (found) return found;
  }
  return null;
}

// The agent's icon from the app's program table: a one-color glyph or a picture.
function icon(p, size, color) {
  if (!p?.icon) return div().size(size).flex_none();
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

export default class WakeSessions extends View {
  init(_props, cx) {
    // A copy opened as the copy menu (openMenu) is only that menu.
    if (props()?.copyMenu || props()?.machineMenu) {
      this.at = Math.max(0, (props().options ?? []).findIndex(o => o.key === props().current));
      this.handle = cx.focus_handle(); this.handle.focus(); return;
    }
    this.local = []; this.remote = new Map(); // this computer's rows; machine -> { state, rows?, home? }
    this.inflight = new Set(); this.listingMachines = false; this.remoteAt = 0; this.merged = null;
    this.scope = 'space'; this.scopeChosen = false; this.loading = false; this.loaded = false;
    this.error = ''; this.pending = false; this.lastLaunch = 0;
    this.launched = new Map(); // row key -> when the panel opened it: hidden until the list catches up
    this.opened = null; // { session, summary, summaryLoading, error }
    this.listScroll = VirtualListScrollHandle.new();
    this.pointerY = null; // the pointer over the list, in list px from its first row's top, unscrolled
    this.input = InputState.new({ placeholder: 'Filter sessions…' });
    this.input.on('change', (_e, cx) => cx.notify());
    this.places = {}; this.locating = false; // folder -> { repo, tree }, from git
    this.repos = {}; // folder -> its repository (a worktree's main checkout), from git (null outside one)
    this.programs = {}; // program key -> its entry in the app's program table (name, icon)
    this.bin = null; // the herdr-wake program that answered last
    this.noSummaries = new Set(); // machines whose herdr-wake has no --summary ('' is this computer)
    this.prefetching = new Set(); this.prefetchedAt = new Map(); // by machine ('' is this computer)
    if (checking()) return;
    cx.spawn(async cx => {
      try { this.programs = Object.fromEntries((await knownAgents()).map(p => [p.key, p])); cx.notify(); }
      catch (_e) { /* no icons: the rows keep their place for one */ }
    });
    this.refresh(cx);
    cx.timer.every(15000, cx => {
      if (!this.visible()) return;
      this.refresh(cx, true);
      if (Date.now() - this.remoteAt > REMOTE_EVERY) this.refreshRemotes(cx);
    });
  }
  // Every row of every machine that answered, newest first (cached until a machine's rows change).
  get rows() {
    return this.merged ??= mergeRows(new Map([[null, this.local], ...[...this.remote].map(([m, c]) => [m, c.rows ?? []])]));
  }
  // Machines that gave a list (even an empty one).
  answered() { return [...this.remote].filter(([, c]) => c.state === 'ok').map(([m]) => m); }
  // Every machine Herdr knows: the picker's choices (one that gave no list says why).
  machines() { return [...this.remote.keys()]; }
  machineNotes() {
    return new Map([...this.remote].flatMap(([m, c]) => { const n = c.state === 'ok' ? null : machineNote(c); return n ? [[m, n]] : []; }));
  }
  pick() { return effectivePick(shared(PICK), this.machines()); }
  // Runs herdr-wake: ~/.local/bin/herdr-wake first (the app may not have it on PATH), then PATH.
  async wake(args, options = {}) {
    const candidates = this.bin ? [this.bin] : [home() + '/.local/bin/herdr-wake', 'herdr-wake'];
    let last;
    for (const bin of candidates) {
      try { const r = await exec(bin, args, options); this.bin = bin; return r; } catch (e) { last = e; }
    }
    throw new Error(`Cannot run herdr-wake: ${String(last?.message ?? last)}`);
  }
  // Runs herdr-wake on `machine` (null: here). Another machine's goes through `hc exec --on` as an argv
  // (see remoteArgs), ended after the options' timeout; what hc itself reports reads as a short sentence.
  // `cx`: the one the calling task got (it sets the timer that gives up on a stuck connection).
  async wakeOn(cx, machine, args, options = {}) {
    if (!machine) return this.wake(args, options);
    const ms = options.timeout ?? 60000;
    const r = await withTimeout(hc(remoteArgs(machine, args, Math.ceil(ms / 1000))), ms + 5000,
      { code: 124, stdout: '', stderr: JSON.stringify({ error: { kind: 'timeout' } }) }, afterOf(cx));
    return remoteResult(machine, r);
  }
  // Shown now? Not while a node on the way is hidden, or a stack on the way shows another child
  // (the tab bar's side panel shows Files, Changes or this).
  visible() {
    const path = panelPath(layout());
    return Boolean(path) && path.every((node, i) => !node.hidden &&
      (node.type !== 'stack' || (node.active ?? node.children?.[0]?.id) === path[i + 1]?.id));
  }
  // `quiet`: a timer reload does not show the spinner or clear a message from a resume.
  refresh(cx, quiet = false) {
    if (!quiet) this.refreshRemotes(cx);
    if (this.loading) return;
    this.loading = true; if (!quiet) this.error = ''; if (!this.rendering) cx.notify();
    cx.spawn(async cx => {
      try {
        const r = await this.wake(['resurrect', '--json', '--limit', '0'], { timeout: 60000 });
        if (r.code !== 0) throw new Error(cleanError(r.stderr));
        const list = JSON.parse(r.stdout).sessions;
        this.local = list.map(j => normalizeRow(j)).filter(s => typeof s.id === 'string').sort((a, b) => b.activity - a.activity);
        this.merged = null; this.loaded = true;
        for (const [id, at] of this.launched) if (Date.now() - at > 90000) this.launched.delete(id);
        this.error = this.error && quiet ? this.error : '';
        this.locate(cx);
        this.prefetch(null, cx);
      } catch (e) { this.error = cleanError(e?.message ?? e); }
      this.loading = false; cx.notify();
    });
  }
  // Summaries for the newest sessions of a machine (null: this computer), made in the background by
  // its herdr-wake, so opening one shows it at once: the newest 60 here, 20 on another machine. Once
  // every ten minutes per machine; not at all when that herdr-wake has no such option.
  prefetch(machine, cx) {
    const key = machine ?? '';
    if (this.prefetching.has(key) || this.noSummaries.has(key) || Date.now() - (this.prefetchedAt.get(key) ?? 0) < 600000) return;
    this.prefetching.add(key); this.prefetchedAt.set(key, Date.now());
    cx.spawn(async cx => {
      try {
        const r = await this.wakeOn(cx, machine, ['resurrect', '--prefetch-summaries', String(prefetchCount(machine))], { timeout: 1800000 });
        if (r.code === 2) this.noSummaries.add(key);
        else if (r.code === 0) { this.prefetching.delete(key); if (machine) this.loadRemote(machine, cx); else this.refresh(cx, true); }
      } catch (_e) { /* best effort: opening a session still asks for its own */ }
      this.prefetching.delete(key);
    });
  }
  // Asks the machines `hc machine ls` lists, each on its own and in parallel: a slow or failing one
  // only leaves its own quiet note. A machine that is offline has no rows and a note.
  refreshRemotes(cx) {
    if (this.listingMachines) return;
    this.listingMachines = true; this.remoteAt = Date.now();
    cx.spawn(async cx => {
      let list = [];
      try { const r = await hc(['machine', 'ls', '--json']); if (r.code === 0) list = parseMachines(r.stdout); } catch (_e) { /* none */ }
      this.listingMachines = false;
      const names = new Set(list.map(m => m.name));
      for (const m of [...this.remote.keys()]) if (!names.has(m)) this.remote.delete(m);
      this.merged = null;
      for (const m of list) {
        if (!m.connected) this.remote.set(m.name, { state: 'offline' });
        else this.loadRemote(m.name, cx);
      }
      cx.notify();
    });
  }
  // One machine's list, kept until it is asked again. Not asked twice at once: a call that outlived
  // its 15 seconds is still running on that machine until it ends.
  loadRemote(machine, cx) {
    if (this.inflight.has(machine)) return;
    this.inflight.add(machine);
    const call = args => { const p = hc(args); p.finally(() => this.inflight.delete(machine)).catch(() => {}); return p; };
    cx.spawn(async cx => {
      const c = await loadMachine(machine, call, 15000, afterOf(cx)), before = this.remote.get(machine);
      if (c.state === 'ok') this.remote.set(machine, { state: 'ok', rows: c.rows, home: guessHome(c.rows.map(s => s.cwd)) });
      // A passing failure (slow, unreachable) keeps the last list and says so in the footer.
      else if (before?.state === 'ok' && ['slow', 'unreachable', 'error'].includes(c.state)) this.remote.set(machine, { ...before, stale: c });
      else this.remote.set(machine, { state: c.state, text: c.text });
      this.merged = null;
      if (c.state === 'ok') this.prefetch(machine, cx);
      cx.notify();
    });
  }
  // "repo > worktree" for every session folder that still exists and every space's folder (to find
  // the space a session ran in), asked of git once per folder, one at a time. Until it answers, and
  // for folders that are gone, the path tells what it can (placeFromPath).
  locate(cx) {
    if (this.locating) return;
    const want = [...this.local.filter(s => s.exists && s.cwd).map(s => s.cwd), ...spaceFolders(spaces(), panes())];
    const folders = [...new Set(want.filter(f => !(f in this.places)))];
    if (!folders.length) { this.chooseScope(); return; }
    this.locating = true;
    cx.spawn(async cx => {
      for (const cwd of folders) {
        try {
          const r = await exec('git', ['rev-parse', '--path-format=absolute', '--git-common-dir', '--show-toplevel'], { cwd, timeout: 5000 });
          this.places[cwd] = r.code === 0 ? placeFromGit(r.stdout, cwd) : placeFromPath(cwd, home());
          this.repos[cwd] = r.code === 0 ? repoFromGit(r.stdout) : null;
        } catch (_e) { this.places[cwd] = placeFromPath(cwd, home()); }
      }
      this.locating = false; this.chooseScope(); cx.notify();
    });
  }
  // The first scope shown: This space when it is a repository or folder of its own with sessions in
  // it; otherwise All spaces (a space at home would hold nearly everything, and an empty
  // This space helps nobody). Decided once, when the places are known; the person's pick wins.
  chooseScope() {
    if (this.scopeChosen || !this.loaded) return;
    this.scopeChosen = true;
    const f = focused(), space = spaces().find(p => p.id === f?.space) ?? spaces().find(p => p.active);
    if (!space || space.machine) { this.scope = 'all'; return; }
    const where = { ...spaceScope(space, panes(), this.repos, home()), machine: null };
    this.scope = where.wide || filterSessions(this.local, '', where, this.repos).length === 0 ? 'all' : 'space';
  }
  // Where a session ran: git's answer for this computer's folders; for another machine's (nothing
  // there is read, no git), what the path tells, with that machine's home guessed from its sessions.
  place(s) {
    if (s.machine) return placeFromPath(s.cwd, this.remote.get(s.machine)?.home ?? '');
    return this.places[s.cwd] ?? placeFromPath(s.cwd, home());
  }
  // The place as the list shows it: this computer's spaces name places; another machine's row says
  // its machine first.
  label(s, names) { return labelPlace(this.place(s), s.machine ? new Map() : names, s.machine); }
  // What the scope shows, for filterSessions: This folder is exactly the focused terminal's folder,
  // This space the focused space's repository with its worktrees, or its folder (see spaceScope), All
  // spaces everything (on the machines the picker chose). The first two are the folders of the
  // focused space's machine and show only that machine's sessions; for another machine nothing is
  // read from git, so its space is its folder.
  where() {
    if (this.scope === 'all') return null;
    const f = focused(), space = spaces().find(p => p.id === f?.space) ?? spaces().find(p => p.active);
    const machine = space?.machine ?? null;
    if (this.scope === 'folder' || !space) return { folder: f?.cwd ?? null, machine };
    return { ...spaceScope(space, panes(), machine ? {} : this.repos, machine ? '' : home()), machine };
  }
  // A click shows the session in the list's place; only Resume or Fork opens it.
  open(s, cx) {
    const d = this.opened = { session: s, summary: s.summary ?? null, summarySource: s.summarySource,
      summaryLoading: !s.summary && validId(s.id) && !this.noSummaries.has(s.machine ?? ''), error: '' };
    cx.notify();
    if (!d.summaryLoading) return;
    cx.spawn(async cx => {
      try {
        const r = await this.wakeOn(cx, s.machine, ['resurrect', '--summary', s.id], { timeout: 180000 });
        if (r.code === 2) this.noSummaries.add(s.machine ?? '');
        else if (r.code === 0) {
          const j = JSON.parse(r.stdout);
          if (typeof j.summary === 'string' && j.summary.trim()) { d.summary = j.summary; d.summarySource = j.summary_source; }
        }
      } catch (_e) { /* no summary: the block stays away */ }
      d.summaryLoading = false; if (this.opened === d) cx.notify();
    });
  }
  back(cx) { this.opened = null; cx.notify(); }
  // The copy menu at the pointer: session ID, resume command, folder. The resume command is asked of
  // the session's own herdr-wake (`resurrect --id <ID> --print`, the same checks as Resume); for another
  // machine it reads "on <machine>: cd <folder> && <command>", a command for that machine. Another copy
  // of this mod draws the menu (menu) from its props.
  openMenu(s, cx) {
    cx.spawn(async cx => {
      let command = null;
      try {
        if (!validId(s.id)) throw new Error('invalid id');
        const r = await this.wakeOn(cx, s.machine, ['resurrect', '--id', s.id, '--print'], { timeout: 15000 });
        if (r.code === 0) command = r.stdout.trim().split('\n')[0] || null;
        else notify({ title: 'Resume command not available', body: cleanError(r.stderr), level: 'warning', timeout: 4000 });
      } catch (_e) { /* the row reads "not found" */ }
      if (command && s.machine) command = `on ${s.machine}: ${command}`;
      const where = placeText(this.label(s, spacePlaces(spaces(), panes(), this.places))), about = s.title;
      openOverlay({ mod: MOD, kind: 'popup', width: 340, height: 26 + COPIES.length * 31 + 12,
        props: { copyMenu: true, title: about && about !== where ? `${where} · ${about}` : where, values: copyValues(s, command) } });
    });
  }
  // The machine picker's menu at the pointer: it writes the choice to shared state, which the panel reads.
  openMachineMenu() {
    const options = machineOptions(this.machines(), this.machineNotes());
    openOverlay({ mod: MOD, kind: 'popup', width: 220, height: 12 + options.length * 31,
      props: { machineMenu: true, options, current: this.pick() } });
  }
  choose(item, cx) {
    const p = props(), value = p.values[item.key];
    if (!value) return;
    cx.write_to_clipboard(value);
    closeOverlay(p.overlay);
    notify({ title: item.label.replace(/^Copy/, 'Copied'), body: value, level: 'success', timeout: 2500 });
  }
  machineMenu(t) {
    const p = props(), options = p.options ?? [];
    const pick = (i, _cx) => { setShared(PICK, options[i].key); closeOverlay(p.overlay); };
    return v_flex().id('machine-menu').track_focus(this.handle).w_full().p(5).bg(t.popover).text_size(12.8)
      .on_key_down((e, cx) => {
        if (e.key === 'down') this.at = (this.at + 1) % options.length;
        else if (e.key === 'up') this.at = (this.at + options.length - 1) % options.length;
        else if (e.key === 'enter') pick(this.at, cx);
        else if (e.key === 'escape') closeOverlay(p.overlay);
        else return;
        cx.notify();
      })
      .children(options.map((o, i) => h_flex().id(`machine-${i}`).h(31).px(8).gap(9).rounded(7).items_center()
        .text_color(t.foreground).when(i === this.at, el => el.bg(t.selected)).hover(el => el.bg(t.hover))
        .on_click((_e, cx) => pick(i, cx))
        .child(div().w(12).flex_none().text_color(t.muted).child(o.key === p.current ? '✓' : ''))
        .child(div().flex_1().min_w(0).truncate().child(o.label))));
  }
  menu(t) {
    const p = props(), pick = (i, cx) => this.choose(COPIES[i], cx);
    return v_flex().id('copy-menu').track_focus(this.handle).w_full().p(5).bg(t.popover).text_size(12.8)
      .on_key_down((e, cx) => {
        if (e.key === 'down') this.at = (this.at + 1) % COPIES.length;
        else if (e.key === 'up') this.at = (this.at + COPIES.length - 1) % COPIES.length;
        else if (e.key === 'enter') pick(this.at, cx);
        else if (e.key === 'escape') closeOverlay(p.overlay);
        else return;
        cx.notify();
      })
      .child(div().px(8).pt(4).pb(6).text_size(11).text_color(t.dim).truncate().child(p.title))
      .children(COPIES.map((it, i) => {
        const value = p.values[it.key], off = !value;
        return h_flex().id(`copy-${it.key}`).h(31).px(8).gap(9).rounded(7).items_center()
          .text_color(off ? t.dim : t.foreground)
          .when(i === this.at && !off, el => el.bg(t.selected))
          .when(!off, el => el.hover(s => s.bg(t.hover)).on_click((_e, cx) => pick(i, cx)))
          .child(svg(`mods/${MOD}/${it.icon}.svg`).size(14).flex_none().text_color(off ? t.dim : t.muted))
          .child(div().flex_none().child(it.label))
          .child(div().flex_1().min_w(0).truncate().text_right().text_size(11.5).text_color(t.dim).child(value ?? 'not found'));
      }));
  }
  // Where Resume opens a session: the focused space when it is on the session's machine, else the first
  // space there; null when that machine has none (a remote one is made, see launch).
  targetSpace(s) {
    const f = focused(), focusedSpace = spaces().find(p => p.id === f?.space) ?? spaces().find(p => p.active);
    return resumeSpace(s.machine ?? null, focusedSpace, spaces());
  }
  // Resume (or Fork, a copy of a Claude session): the session's own herdr-wake (on its machine, through
  // `hc exec --on`) checks it and writes a launch ticket (`--ticket`, nothing is opened); the panel then
  // opens the tab itself, named after the title, in a space on that machine and the session's folder, and
  // runs the ticket in it (a terminal of a space on a machine starts there). A machine with no space gets
  // one, named after it, whose first tab runs the ticket. A refusal's text is shown.
  launch(s, fork, cx) {
    if (this.pending || Date.now() - this.lastLaunch < 2000) return;
    const d = this.opened, machine = s.machine ?? null;
    const fail = text => { if (d) d.error = text; else this.error = text; };
    if (d) d.error = '';
    const space = this.targetSpace(s);
    if (!space && !machine) { fail('Open a space on this computer to open the session in.'); cx.notify(); return; }
    const spaceId = space ? localSpaceId(space.id, machine) : null;
    if (spaceId && !validSpaceId(spaceId)) { fail('This space has an id herdr-wake cannot use.'); cx.notify(); return; }
    this.pending = true; cx.notify();
    cx.spawn(async cx => {
      try {
        const args = ['resurrect', '--id', s.id, ...(spaceId ? ['--space', spaceId] : []), '--ticket', ...(fork ? ['--fork'] : [])];
        const r = await this.wakeOn(cx, machine, args, { timeout: 60000 });
        if (r.code !== 0) throw new Error(cleanError(r.stderr));
        const t = parseTicket(r.stdout), name = t.title ?? `${harnessName(s)} session`;
        if (space) run('tab.new', { space: space.id, folder: t.cwd, name, argv: t.argv });
        else run('space.new', { machine, name: machine, folder: t.cwd, tab: name, argv: t.argv });
        this.lastLaunch = Date.now();
        if (!fork) this.launched.set(rowKey(s), Date.now());
        // It runs now, so it leaves the list of closed sessions: back to the list.
        if (this.opened === d) this.opened = null;
        cx.timer.after(3000, cx => { this.refresh(cx, true); if (machine) this.loadRemote(machine, cx); });
      } catch (e) { fail(cleanError(e?.message ?? e)); }
      this.pending = false; cx.notify();
    });
  }
  render(cx) {
    const t = theme();
    if (props()?.copyMenu) return this.menu(t);
    if (props()?.machineMenu) return this.machineMenu(t);
    // Opening the panel again reloads the list: sessions may have closed meanwhile.
    const shown = this.visible();
    if (shown && this.shown === false && !checking()) {
      this.rendering = true;
      try { this.refresh(cx); } finally { this.rendering = false; }
    }
    this.shown = shown;
    if (shown && !checking()) this.locate(cx);
    const f = focused();
    const query = this.input.value();
    const pick = this.pick();
    const rows = filterSessions(this.rows, query, this.where(), this.repos, pick).filter(s => !this.launched.has(rowKey(s)));
    const count = this.pending ? 'Opening…' : this.loading && !this.rows.length ? '' : `${rows.length} closed`;
    // The card, header and list in the style of Herdr's side panels.
    const card = v_flex().id('wake-sessions-root').size_full().min_h(0).rounded(10).overflow_hidden()
      .bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13);
    const d = this.opened;
    if (d) {
      const s = d.session, space = this.targetSpace(s);
      const place = this.label(s, spacePlaces(spaces(), panes(), this.places));
      const spaceName = space?.name ?? (s.machine ? `new space on ${s.machine}` : undefined);
      return card.child(sessionDetail(s, { ...d, pending: this.pending, spaceName, place }, t, {
        back: (_e, cx) => this.back(cx), resume: (_e, cx) => this.launch(s, false, cx),
        fork: (_e, cx) => this.launch(s, true, cx), copy: (_e, cx) => this.openMenu(s, cx) }));
    }
    // The machine picker shows once Herdr knows another machine; it sits under the scope control (the
    // panel is too narrow for both in one line) and takes the count with it.
    const picker = this.machines().length > 0;
    const countText = div().flex_1().min_w(0).truncate().text_right().text_size(12).text_color(t.dim).child(count);
    const notes = noteLines([...this.remote].sort(([a], [b]) => (a < b ? -1 : 1)).flatMap(([machine, c]) => {
      const n = c.state === 'ok' ? (c.stale ? machineNote(c.stale) : null) : machineNote(c);
      return n ? [{ machine, text: n }] : [];
    }));
    return card
      .child(v_flex().flex_none().border_b(1).border_color(t.border)
        .child(h_flex().h(42).flex_none().px(10).gap(8).items_center()
          .child(segmented('scope', SCOPES, this.scope, (k, cx) => { this.scope = k; this.scopeChosen = true; cx.notify(); }, t))
          .when(!picker, el => el.child(countText))
          .when(picker, el => el.child(div().flex_1()))
          .child(iconButton('reload', `mods/${MOD}/refresh.svg`, (_e, cx) => this.refresh(cx), t, this.loading || this.inflight.size > 0)))
        .when(picker, el => el.child(h_flex().h(34).flex_none().px(10).pb(8).gap(8).items_center()
          .child(div().w(150).flex_none().flex()
            .child(pickerButton('machine-pick', machineOptions(this.machines()).find(o => o.key === pick)?.label ?? 'All machines',
              (_e, _cx) => this.openMachineMenu(), t)))
          .child(countText))))
      .child(v_flex().flex_none().px(10).pt(10)
        .child(h_flex().h(30).flex_none().px(8).items_center().rounded(8).border_1().border_color(t.border).bg(t.background)
          .child(Input.new(this.input).flex_1().h_full().text_size(12.5).text_color(t.foreground))))
      .when(Boolean(this.error), el => el.child(div().flex_none().px(12).pt(8).text_size(12).text_color(t.blocked).child(this.error)))
      .child(this.list(rows, query, t, cx, pick))
      // Machines that gave no list say why, once and quietly.
      .when(notes.length > 0, el => el.child(v_flex().flex_none().px(12).py(6).gap(2).border_t(1).border_color(t.border)
        .children(notes.map(text => div().text_size(11).text_color(t.dim).child(text)))));
  }
  // The list: a virtual list, so hundreds of sessions cost what the visible rows cost.
  list(rows, query, t, cx, pick = ALL_MACHINES) {
    if (!rows.length) {
      const why = pick && pick !== ALL_MACHINES ? this.machineNotes().get(pick) : null;
      const text = this.loading ? 'Loading sessions…' : why ? `${pick}: ${why}` : query.trim() ? 'No matches'
        : { all: 'No closed sessions', space: 'No closed sessions in this space. Choose All spaces.',
          folder: 'No closed sessions in this folder. Choose This space or All spaces.' }[this.scope];
      return v_flex().flex_1().min_h(0).p(6).child(div().p(8).text_size(12).text_color(t.dim).child(text));
    }
    const at = Object.fromEntries(rows.map(s => [rowKey(s), s]));
    const names = spacePlaces(spaces(), panes(), this.places);
    // Hover: a row's own .hover() isn't redrawn inside a virtual list, and rows can't have handlers,
    // so the list follows the pointer and says which row is under it.
    const PAD = 6;
    return v_flex().id('history-area').relative().flex_1().min_h(0).p(PAD).cursor_pointer()
      .on_mouse_move((e, cx) => {
        if (!e.local_position) return;
        const before = this.hoverIndex();
        this.pointerY = e.local_position.y / (settings().zoom || 1) - PAD;
        if (this.hoverIndex() !== before) cx.notify();
      })
      .on_hover((on, cx) => { if (!on && this.pointerY != null) { this.pointerY = null; cx.notify(); } })
      .child(v_virtual_list('history-list', rows.length, ROW, i => rowKey(rows[i]),
        range => { const h = this.hoverIndex(); return rows.slice(range.start, range.end).map((s, k) => this.row(s, t, range.start + k === h, names)); })
        .track_scroll(this.listScroll)
        .on_item_click((key, cx) => { if (at[key]) this.open(at[key], cx); })
        // Right-click: the copy menu.
        .on_item_secondary_click((key, _e, cx) => { if (at[key]) this.openMenu(at[key], cx); }))
      .child(Scrollbar.vertical('history-list').absolute().inset_0());
  }
  // The row under the pointer, by index in the shown list; -1 when the pointer is elsewhere.
  hoverIndex() {
    if (this.pointerY == null || this.pointerY < 0) return -1;
    return Math.floor((this.pointerY + this.listScroll.offset().y) / ROW);
  }
  // One row: the agent's icon and what the session was about (its summary, else its title); under it,
  // small and dim, where it ran ("machine · repo > worktree"); then how long ago and a mark. Drawn
  // inside the virtual list, so no handlers here (the list has them).
  row(s, t, hovered = false, names = new Map()) {
    const state = sessionState(s).key;
    const where = placeLine(this.label(s, names), state === 'missing', t, true);
    return h_flex().h(ROW).flex_none().px(8).gap(9).items_center().rounded(8).when(hovered, el => el.bg(t.hover))
      .child(v_flex().flex_1().min_w(0).gap(2)
        .child(h_flex().min_w(0).gap(9).items_center()
          .child(div().flex_none().relative().top(1).child(icon(this.programs[s.program ?? 'claude'], 16, t.foreground)))
          .child(div().flex_1().min_w(0).truncate().text_color(t.foreground).child(headline(s))))
        .child(div().min_w(0).pl(25).text_size(11.5).child(where)))
      .child(div().flex_none().text_size(11).text_color(t.dim).child(ago(s.activity)))
      // Left mid-work: a yellow dot. Otherwise the place stays empty.
      .child(div().size(7).mx(3).flex_none().rounded(4).when(state === 'stopped', el => el.bg(t.ansi3)));
  }
}
