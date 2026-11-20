import { defineScene } from '@lincyaw/addon-motion/scene'

export default defineScene(({ root, at, durations, parameters }) => {
  const copy = parameters.copy as Record<string, any>
  const palette = parameters.palette as Record<string, string>
  for (const [name, value] of Object.entries(palette)) root.style.setProperty(`--pm-color-${name}`, value)
  const esc = (s: string) => s.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!))
  const panel = (x: number, y: number, w: number, h: number, name: string, role: string, extra = '') => `
    <g class="machine" transform="translate(${x} ${y})">
      <rect width="${w}" height="${h}" rx="13" fill="var(--pm-color-surface)" stroke="var(--pm-color-line)"/>
      <path d="M18 44H${w - 18}" stroke="var(--pm-color-line)"/>
      <circle cx="22" cy="23" r="4" fill="var(--pm-color-muted)"/>
      <text x="38" y="29" class="device">${esc(name)}</text>
      <rect x="18" y="65" width="${w - 36}" height="56" rx="6" fill="var(--pm-color-bg)"/>
      <text x="32" y="101" class="role">&gt;_ ${esc(role)}</text>${extra}
      <circle class="online" cx="${w - 24}" cy="23" r="5" fill="var(--pm-color-ok)" opacity="0"/>
    </g>`
  root.innerHTML = `<style>
    .mesh-frame { width:100%;height:100%;background:var(--pm-color-bg);color:var(--pm-color-ink);font-family:'IBM Plex Sans',system-ui,sans-serif; }
    .mesh-frame svg {display:block;width:100%;height:100%;}
    .mesh-frame text {fill:var(--pm-color-ink)}
    .mesh-frame .device {font-size:21px;font-weight:500}
    .mesh-frame .role {font:23px 'IBM Plex Mono',monospace}
    .mesh-frame .micro {font:16px 'IBM Plex Mono',monospace;fill:var(--pm-color-muted);letter-spacing:1.3px}
    .mesh-frame .caption {font-size:27px;font-weight:500}
    .mesh-frame .command {font:23px 'IBM Plex Mono',monospace;fill:var(--pm-color-accent)}
  </style><div class="mesh-frame"><svg viewBox="0 0 1000 625" role="img" aria-label="${esc(copy.alt)}">
    <text x="42" y="43" style="font-size:25px;font-weight:600;letter-spacing:-1px">${esc(copy.brand)}</text>
    <text x="958" y="41" text-anchor="end" class="micro">${esc(copy.illustration)}</text>
    <text class="headline caption" x="42" y="98">${esc(copy.headlines[0])}</text>
    <g class="routes" fill="none" stroke-width="2.5">
      <path class="base" d="M342 293C470 293 500 183 665 183" stroke="var(--pm-color-line)" stroke-dasharray="5 7"/>
      <path class="base" d="M342 340C470 340 500 457 665 457" stroke="var(--pm-color-line)" stroke-dasharray="5 7"/>
      <path class="upper wire" d="M342 293C470 293 500 183 665 183" pathLength="1" stroke="var(--pm-color-accent)" stroke-dasharray="1 2"/>
      <path class="lower wire" d="M342 340C470 340 500 457 665 457" pathLength="1" stroke="var(--pm-color-accent)" stroke-dasharray="1 2"/>
    </g>
    ${panel(42, 180, 300, 275, copy.devices[0], copy.roles[0], `<rect x="18" y="132" width="264" height="56" rx="6" fill="var(--pm-color-bg)"/><text x="32" y="168" class="role">&gt;_ ${esc(copy.roles[1])}</text><text x="20" y="242" class="micro">${esc(copy.local)}</text>`)}
    ${panel(665, 130, 290, 170, copy.devices[1], copy.roles[2], `<text x="20" y="151" class="micro status">${esc(copy.offline)}</text>`)}
    ${panel(665, 372, 290, 170, copy.devices[2], copy.roles[3], `<text x="20" y="151" class="micro status">${esc(copy.offline)}</text>`)}
    <g class="invite" opacity="0"><rect x="-67" y="-22" width="134" height="44" rx="7" fill="var(--pm-color-accent)"/><text x="0" y="7" text-anchor="middle" style="font:18px 'IBM Plex Mono',monospace;fill:var(--pm-color-bg)">${esc(copy.invite)}</text></g>
    <g class="packet" opacity="0"><rect x="-10" y="-7" width="20" height="14" rx="3" fill="var(--pm-color-accent)"/></g>
    <g class="broadcast-a" opacity="0"><rect x="-10" y="-7" width="20" height="14" rx="3" fill="var(--pm-color-accent)"/></g>
    <g class="broadcast-b" opacity="0"><rect x="-10" y="-7" width="20" height="14" rx="3" fill="var(--pm-color-accent)"/></g>
    <g class="received" opacity="0"><rect x="690" y="315" width="126" height="34" rx="5" fill="var(--pm-color-accent)"/><text x="753" y="338" text-anchor="middle" style="font-size:19px;fill:var(--pm-color-bg)">${esc(copy.received)}</text></g>
    <text class="command" x="42" y="592">${esc(copy.commands[0])}</text>
  </svg></div>`
  const get = (s: string) => root.querySelector(s) as SVGElement
  const upper = get('.upper') as SVGPathElement, lower = get('.lower') as SVGPathElement
  const clamp = (p: number) => Math.max(0, Math.min(1, p))
  const ease = (p: number) => { p = clamp(p); return p * p * (3 - 2 * p) }
  const local = (ms: number, k: number) => clamp((ms - at(k)) / durations[k]!)
  const move = (selector: string, path: SVGPathElement, p: number, visible: boolean) => {
    const point = path.getPointAtLength(path.getTotalLength() * clamp(p))
    const node = get(selector)
    node.setAttribute('transform', `translate(${point.x} ${point.y})`)
    node.setAttribute('opacity', visible ? '1' : '0')
  }
  return {
    seek(ms) {
      const phase = ms > at(3) ? 3 : ms > at(2) ? 2 : ms > at(1) ? 1 : 0
      const t = local(ms, phase)
      const join = ease((ms - at(1) - durations[1]! * .42) / (durations[1]! * .28))
      get('.headline').textContent = copy.headlines[phase]
      get('.command').textContent = copy.commands[phase]
      upper.setAttribute('stroke-dashoffset', String(1 - join))
      lower.setAttribute('stroke-dashoffset', '0')
      root.querySelectorAll('.online').forEach((n, i) => n.setAttribute('opacity', i === 1 ? String(join) : '1'))
      root.querySelectorAll('.status').forEach((n, i) => n.textContent = i > 0 || join > .98 ? copy.online : copy.offline)
      const invite = get('.invite')
      const ip = ease(t / .42)
      const invitePoint = upper.getPointAtLength(upper.getTotalLength() * ip)
      invite.setAttribute('transform', `translate(${invitePoint.x} ${invitePoint.y})`)
      invite.setAttribute('opacity', phase === 1 && t < .47 ? '1' : '0')
      const privateP = t < .5 ? ease((t - .05) / .32) : 1 - ease((t - .51) / .29)
      move('.packet', upper, privateP, phase === 2 && t > .04 && t < .81)
      get('.received').setAttribute('opacity', phase === 2 && t > .38 ? '1' : '0')
      move('.broadcast-a', upper, ease((t - .05) / .48), phase === 3 && t > .05 && t < .58)
      move('.broadcast-b', lower, ease((t - .05) / .48), phase === 3 && t > .05 && t < .58)
      root.querySelectorAll('.machine').forEach((n, i) => {
        const active = phase === 3 && t > .54 || phase === 2 && (i === 1 && t > .36 || i === 0 && t > .8)
        n.querySelector('rect')!.setAttribute('stroke', active ? 'var(--pm-color-accent)' : 'var(--pm-color-line)')
      })
    },
    dispose() { root.innerHTML = '' },
  }
})
