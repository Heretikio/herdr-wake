// The session view of the herdr-wake Sessions panel. Adapted in part from inlet/herdr-sessions (MIT).
import { div } from 'gpui-kit';
import { v_flex, h_flex } from 'gpui-base';
import { home } from 'herdr';
import { formatSessionTime, compactSummary, ago, formatDuration, formatCost, modelName, sessionState, stateColor,
  harnessName, cannotResume, canFork } from './logic.mjs';
import { button, placeLine, branchIcon } from './ui.js';

// The full folder, with home as ~.
function folder(path) {
  const h = home().replace(/\/+$/, '');
  if (typeof path !== 'string' || !path) return 'Unknown';
  return path === h || path.startsWith(h + '/') ? '~' + path.slice(h.length) : path;
}

/**
 * One session in the panel, in the list's place: a header back to the list, what it was about, Resume,
 * Fork and the copy menu, then everything herdr-wake knows about it. Nothing happens to the session
 * until Resume is pressed.
 * `st`: { summary, summaryLoading, error, pending, spaceName, place: { repo, tree, worktree } as the row shows it }.
 * Facts herdr-wake did not give (branch, cost, duration, ...) are left out.
 */
export function sessionDetail(s, st, t, { back, resume, fork, copy }) {
  const fact = (label, value) => h_flex().gap(10).items_start()
    .child(div().w(84).flex_none().text_color(t.dim).child(label))
    .child(div().flex_1().min_w(0).text_color(t.foreground).child(value));
  const started = formatSessionTime(s.started), ended = formatSessionTime(s.endedAt);
  const duration = formatDuration(s.duration), cost = formatCost(s.cost);
  const changes = typeof s.linesAdded === 'number' || typeof s.linesRemoved === 'number';
  const prs = s.prs ?? [], models = (s.models ?? []).map(modelName);
  const why = cannotResume(s), busy = st.pending ? 'Opening…' : null;
  const state = sessionState(s);
  const summary = compactSummary(st.summary ?? s.summary);
  const showSummary = st.summaryLoading || summary;
  return v_flex().size_full().min_h(0)
    .child(h_flex().h(42).flex_none().px(10).gap(8).items_center().border_b(1).border_color(t.border)
      .child(div().id('back').w(20).flex_none().cursor_pointer().text_color(t.muted).hover(x => x.text_color(t.foreground))
        .on_click(back).child('←'))
      .child(div().flex_1().min_w(0).font_semibold().child(placeLine(st.place ?? { repo: s.title || harnessName(s), tree: null }, !s.exists, t)))
      .child(div().flex_none().text_size(12).text_color(t.dim).child(`${ago(s.activity)} ago`)))
    .child(div().id('session-detail').flex_1().min_h(0).p(14).overflow_y_scrollbar()
      .child(v_flex().gap(12)
        .child(div().text_size(14).font_semibold().child(s.title || `${harnessName(s)} session`))
        .child(h_flex().gap(6).items_center().flex_wrap()
          .child(button('session-resume', st.pending ? 'Opening…' : '▶ Resume session', resume, t, true, why ?? busy))
          .when((s.program ?? 'claude') === 'claude', el => el.child(button('session-fork', 'Fork', fork, t, false, !canFork(s) ? (why ?? 'Not available') : busy)))
          .child(button('session-copy', 'Copy ▾', copy, t)))
        .when(Boolean(st.error), el => el.child(div().text_size(12).text_color(t.blocked).child(st.error)))
        .when(Boolean(showSummary), el => el.child(v_flex().gap(3)
          .child(div().text_size(11).text_color(t.dim).child((st.summarySource ?? s.summarySource) === 'recap' && !st.summaryLoading ? 'Recap' : 'Summary'))
          .child(div().text_size(12.5).text_color(st.summaryLoading && !summary ? t.dim : t.foreground).child(summary ?? 'Loading summary…'))))
        .child(v_flex().gap(6).pt(12).border_t(1).border_color(t.border).text_size(12)
          .child(fact('Status', h_flex().gap(6).items_center()
            .child(div().flex_none().text_color(stateColor(state.key, t)).child('●')).child(state.label)))
          .child(fact('Agent', harnessName(s)))
          .child(fact('Folder', h_flex().gap(6).flex_wrap().child(folder(s.cwd))
            .when(!s.exists, el => el.child(div().text_color(t.ansi1).child('missing')))))
          .when(Boolean(s.branch), el => el.child(fact('Branch', s.branch === 'HEAD' ? 'none (detached HEAD)'
            : h_flex().gap(5).items_center().child(branchIcon(t.dim)).child(div().min_w(0).child(s.branch)))))
          .when(Boolean(st.spaceName), el => el.child(fact('Space', st.spaceName)))
          .child(fact('Last active', formatSessionTime(s.activity * 1000) ?? 'Not available'))
          .when(Boolean(started), el => el.child(fact('Started', started)))
          .when(Boolean(ended), el => el.child(fact('Ended', ended)))
          .when(Boolean(duration), el => el.child(fact('Duration', duration)))
          .when(Boolean(cost), el => el.child(fact('Cost', `${cost} at API rates`)))
          .when(changes, el => el.child(fact('Changes', h_flex().gap(6)
            .child(div().text_color(t.ansi2).child(`+${s.linesAdded ?? 0}`))
            .child(div().text_color(t.ansi1).child(`−${s.linesRemoved ?? 0}`)))))
          .when(models.length > 0, el => el.child(fact('Models', models.join(', '))))
          .when(prs.length > 0, el => el.child(fact('Pull requests', h_flex().gap(4).flex_wrap()
            .children(prs.map((p, i) => div().id(`session-pr-${i}`).px(5).rounded(4).border_1().border_color(t.ansi5).text_color(t.ansi5)
              .cursor_pointer().hover(x => x.bg(t.hover)).on_click((_e, cx) => cx.open_url(p.url))
              .child(`#${p.number ?? '?'}${p.repo ? ' ' + p.repo : ''} ↗`))))))
          // Flag names only, as herdr-wake keeps them; a permission bypass is marked.
          .child(fact('Flags', h_flex().gap(6).flex_wrap()
            .child(div().min_w(0).child(s.flags.length ? s.flags.join(' ') : 'none'))
            .when(s.bypass, el => el.child(div().text_color(t.ansi3).child('! bypasses permissions')))))
          .child(fact('Source', s.source === 'wake' ? 'herdr-wake history' : 'agent transcript'
            + (s.flagsFromTranscript ? ' (flags rebuilt from it)' : '')))
          .child(fact('Session file', div().text_color(s.transcript === 'gone' ? t.ansi1 : t.foreground)
            .child({ present: 'present', gone: 'deleted by the agent', unknown: 'not checked' }[s.transcript] ?? s.transcript)))
          .child(fact('Session ID', div().text_color(t.muted).child(s.id))))));
}
