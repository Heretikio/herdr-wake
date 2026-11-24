// Small pieces of the panel: a segmented control, a header icon button and a button; and the place
// line that a row and a session's header share.

import { div, svg } from 'gpui-kit';
import { h_flex } from 'gpui-base';

// Two options side by side, one chosen: [ This space | All spaces ].
export function segmented(id, options, value, pick, t) {
  return h_flex().flex_none().p(2).gap(2).rounded(8).bg(t.chrome).border_1().border_color(t.border)
    .children(options.map(([key, label]) => div().id(`${id}-${key}`).px(9).py(2).rounded(6).text_size(12).cursor_pointer()
      .bg(key === value ? t.background : t.chrome).text_color(key === value ? t.foreground : t.muted)
      .when(key === value, (el) => el.font_semibold())
      .hover((el) => el.text_color(t.foreground)).on_click((_e, cx) => { if (key !== value) pick(key, cx); }).child(label)));
}

// A square icon button in a header: the icon tinted like the text, turning while `spinning`.
export function iconButton(id, icon, action, t, spinning = false) {
  return div().id(id).size(22).flex_none().flex().items_center().justify_center().rounded(6).cursor_pointer()
    .text_color(t.muted).hover((el) => el.bg(t.hover).text_color(t.foreground)).on_click(action)
    .child(svg(icon).size(15).when(spinning, (el) => el.spin(900)));
}

// A plain button; `primary` draws its border in the accent, `off` greys it out and says why on hover.
export function button(id, child, action, t, primary = false, off = null) {
  const el = h_flex().id(id).gap(6).px(9).py(4).rounded(7).items_center().text_size(12).flex_none()
    .border_1().border_color(primary && !off ? t.accent : t.border);
  if (off) return el.text_color(t.dim).cursor_not_allowed().tooltip(off).child(child);
  return el.cursor_pointer().text_color(primary ? t.foreground : t.muted)
    .hover((e) => e.bg(t.hover).text_color(t.foreground)).on_click(action).child(child);
}

// Where a session ran, as the Agents panel writes a space: "repo › space", the repo grey when a
// space or worktree follows it. `gone`: the folder no longer exists, so it can't be resumed there:
// red and struck through. Used by a row and by the session's header. A worktree gets the branch
// icon before its name, as Herdr's spaces sidebar draws one.
export function placeLine({ repo, tree, worktree }, gone, t) {
  const name = (text, color) => div().text_color(gone ? t.ansi1 : color).when(gone, el => el.line_through()).child(text);
  return h_flex().min_w(0).gap(4).items_center()
    .child(name(repo, tree ? t.muted : t.foreground).flex_none().max_w(tree ? 150 : 240).truncate())
    .when(Boolean(tree), el => el.child(div().flex_none().text_color(gone ? t.ansi1 : t.dim).child('›'))
      .when(Boolean(worktree), el => el.child(branchIcon(gone ? t.ansi1 : t.dim)))
      .child(name(tree, t.foreground).min_w(0).truncate()));
}

// The branch icon at 13px, as Herdr's spaces sidebar draws it before a worktree.
export function branchIcon(color) {
  return svg('mods/herdr-wake-sessions/branch.svg').size(13).flex_none().text_color(color);
}
