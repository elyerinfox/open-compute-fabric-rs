<script setup lang="ts">
// Renders a colored pill for a Health / LifecycleState / DiskHealth-style status.
// Accepts any string; unknown values render neutral.
const props = defineProps<{
  status: string
  /** Render a leading dot only (compact). */
  dot?: boolean
}>()

const STYLES: Record<string, string> = {
  // Health
  healthy: 'bg-emerald-500/15 text-emerald-400 ring-emerald-500/30',
  degraded: 'bg-amber-500/15 text-amber-400 ring-amber-500/30',
  unhealthy: 'bg-rose-500/15 text-rose-400 ring-rose-500/30',
  // LifecycleState
  running: 'bg-emerald-500/15 text-emerald-400 ring-emerald-500/30',
  pending: 'bg-sky-500/15 text-sky-400 ring-sky-500/30',
  creating: 'bg-sky-500/15 text-sky-400 ring-sky-500/30',
  stopping: 'bg-amber-500/15 text-amber-400 ring-amber-500/30',
  stopped: 'bg-slate-500/15 text-slate-400 ring-slate-500/30',
  failed: 'bg-rose-500/15 text-rose-400 ring-rose-500/30',
  // DiskHealth
  ok: 'bg-emerald-500/15 text-emerald-400 ring-emerald-500/30',
  warning: 'bg-amber-500/15 text-amber-400 ring-amber-500/30',
  failing: 'bg-rose-500/15 text-rose-400 ring-rose-500/30',
  // status endpoint
  down: 'bg-rose-500/15 text-rose-400 ring-rose-500/30',
  // Health-finding Severity
  info: 'bg-sky-500/15 text-sky-400 ring-sky-500/30',
  critical: 'bg-rose-500/15 text-rose-400 ring-rose-500/30',
}

const DOT: Record<string, string> = {
  healthy: 'bg-emerald-400',
  running: 'bg-emerald-400',
  ok: 'bg-emerald-400',
  degraded: 'bg-amber-400',
  warning: 'bg-amber-400',
  stopping: 'bg-amber-400',
  unhealthy: 'bg-rose-400',
  failed: 'bg-rose-400',
  failing: 'bg-rose-400',
  down: 'bg-rose-400',
  pending: 'bg-sky-400',
  creating: 'bg-sky-400',
  stopped: 'bg-slate-400',
  info: 'bg-sky-400',
  critical: 'bg-rose-400',
}

const key = computed(() => props.status?.toLowerCase?.() ?? '')
const pillClass = computed(
  () => STYLES[key.value] ?? 'bg-slate-500/15 text-slate-400 ring-slate-500/30',
)
const dotClass = computed(() => DOT[key.value] ?? 'bg-slate-400')
</script>

<template>
  <span
    v-if="dot"
    class="inline-flex items-center gap-1.5 text-xs font-medium text-slate-300"
  >
    <span class="h-2 w-2 rounded-full" :class="dotClass" />
    {{ status }}
  </span>
  <span
    v-else
    class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ring-1 ring-inset"
    :class="pillClass"
  >
    <span class="h-1.5 w-1.5 rounded-full" :class="dotClass" />
    {{ status }}
  </span>
</template>
