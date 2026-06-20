<script setup lang="ts">
// Consistent page heading with optional subtitle and a "mock data" badge
// shown when the data on the page came from bundled fixtures.
defineProps<{
  title: string
  subtitle?: string
  source?: 'live' | 'mock'
}>()
</script>

<template>
  <div class="mb-6 flex flex-wrap items-end justify-between gap-3">
    <div>
      <h1 class="text-2xl font-semibold tracking-tight text-slate-50">{{ title }}</h1>
      <p v-if="subtitle" class="mt-1 text-sm text-slate-400">{{ subtitle }}</p>
    </div>
    <div class="flex items-center gap-2">
      <span
        v-if="source === 'mock'"
        class="inline-flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-xs font-medium text-amber-400 ring-1 ring-inset ring-amber-500/30"
        title="ocf-api is unreachable — showing bundled mock fixtures"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-amber-400" />
        mock data
      </span>
      <span
        v-else-if="source === 'live'"
        class="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-400 ring-1 ring-inset ring-emerald-500/30"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-emerald-400" />
        live
      </span>
      <slot name="actions" />
    </div>
  </div>
</template>
