<script setup lang="ts">
// A summary metric card used on the dashboard.
defineProps<{
  label: string
  value: string | number
  /** Optional sub-line under the value. */
  hint?: string
  /** Tailwind accent color name for the icon chip, e.g. 'emerald', 'brand'. */
  accent?: string
}>()

const ACCENTS: Record<string, string> = {
  brand: 'bg-brand-600/15 text-brand-400',
  emerald: 'bg-emerald-500/15 text-emerald-400',
  amber: 'bg-amber-500/15 text-amber-400',
  rose: 'bg-rose-500/15 text-rose-400',
  sky: 'bg-sky-500/15 text-sky-400',
  violet: 'bg-violet-500/15 text-violet-400',
}
</script>

<template>
  <div class="ocf-card p-5">
    <div class="flex items-start justify-between gap-3">
      <div>
        <p class="text-sm font-medium text-slate-400">{{ label }}</p>
        <p class="mt-2 text-3xl font-semibold tracking-tight text-slate-50">{{ value }}</p>
        <p v-if="hint" class="mt-1 text-xs text-slate-500">{{ hint }}</p>
      </div>
      <div
        v-if="$slots.icon"
        class="flex h-10 w-10 items-center justify-center rounded-lg"
        :class="ACCENTS[accent ?? 'brand'] ?? ACCENTS.brand"
      >
        <slot name="icon" />
      </div>
    </div>
  </div>
</template>
