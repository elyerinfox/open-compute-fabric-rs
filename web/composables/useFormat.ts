// Small shared formatting helpers used across pages.

export function useFormat() {
  function bytes(n: number | null | undefined): string {
    if (n == null) return '—'
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB']
    let v = n
    let i = 0
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024
      i++
    }
    const precision = v >= 100 || i === 0 ? 0 : 1
    return `${v.toFixed(precision)} ${units[i]}`
  }

  function bitsPerSec(n: number | null | undefined): string {
    if (n == null) return '—'
    const units = ['bps', 'Kbps', 'Mbps', 'Gbps', 'Tbps']
    let v = n
    let i = 0
    while (v >= 1000 && i < units.length - 1) {
      v /= 1000
      i++
    }
    return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${units[i]}`
  }

  function millicores(n: number | null | undefined): string {
    if (n == null) return '—'
    return n >= 1000 ? `${(n / 1000).toFixed(n % 1000 === 0 ? 0 : 1)} vCPU` : `${n}m`
  }

  function number(n: number | null | undefined): string {
    if (n == null) return '—'
    return n.toLocaleString('en-US')
  }

  function date(s: string | null | undefined): string {
    if (!s) return '—'
    const d = new Date(s)
    if (Number.isNaN(d.getTime())) return s
    return d.toLocaleDateString('en-US', { year: 'numeric', month: 'short', day: 'numeric' })
  }

  return { bytes, bitsPerSec, millicores, number, date }
}
