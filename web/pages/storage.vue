<script setup lang="ts">
const api = useApi()
const { bytes, date } = useFormat()

const { data } = await useAsyncData('storage', () => api.listDisks())
const disks = computed(() => data.value?.data ?? [])
const source = computed<'live' | 'mock'>(() => data.value?.source ?? 'mock')

const failing = computed(() => disks.value.filter((d) => d.health === 'Failing').length)
const warning = computed(() => disks.value.filter((d) => d.health === 'Warning').length)
const rmad = computed(() => disks.value.filter((d) => d.rma_date).length)

const LED_STYLE: Record<string, string> = {
  Normal: 'text-slate-500',
  Locate: 'text-sky-400 animate-pulse',
  Fault: 'text-rose-400',
  Rebuild: 'text-amber-400 animate-pulse',
}
</script>

<template>
  <div>
    <PageHeader
      title="Storage"
      subtitle="Physical disks across the fleet — health, SMART, LED, and RMA tracking."
      :source="source"
    />

    <!-- Summary -->
    <div class="mb-6 grid grid-cols-2 gap-4 sm:grid-cols-4">
      <StatCard label="Total Disks" :value="disks.length" accent="brand" />
      <StatCard label="Warning" :value="warning" :accent="warning ? 'amber' : 'emerald'" />
      <StatCard label="Failing" :value="failing" :accent="failing ? 'rose' : 'emerald'" />
      <StatCard label="RMA'd" :value="rmad" accent="violet" />
    </div>

    <ResourceTable
      :columns="[
        { key: 'dev', label: 'Device', mono: true },
        { key: 'model', label: 'Model' },
        { key: 'serial', label: 'Serial', mono: true },
        { key: 'machine', label: 'Machine' },
        { key: 'size', label: 'Size', align: 'right' },
        { key: 'health', label: 'Health' },
        { key: 'led', label: 'LED' },
        { key: 'rma', label: 'RMA' },
        { key: 'actions', label: '', align: 'right' },
      ]"
      :rows="disks"
      :row-key="(d) => d.metadata.id"
      empty="No physical disks reported."
    >
      <template #cell-dev="{ row }">
        <div>
          <p class="text-slate-300">{{ row.dev_path }}</p>
          <p class="text-[10px] text-slate-600">
            {{ row.enclosure ? `${row.enclosure} · slot ${row.slot}` : '—' }}
          </p>
        </div>
      </template>
      <template #cell-model="{ row }">
        <div>
          <p class="text-slate-200">{{ row.model }}</p>
          <p class="text-xs text-slate-500">{{ row.vendor }}</p>
        </div>
      </template>
      <template #cell-machine="{ row }">
        <span class="font-mono text-xs text-slate-400">{{ row.machine_name ?? row.machine_id }}</span>
      </template>
      <template #cell-size="{ row }">
        <span class="text-slate-300">{{ bytes(row.size_bytes) }}</span>
      </template>
      <template #cell-health="{ row }">
        <HealthBadge :status="row.health" />
      </template>
      <template #cell-led="{ row }">
        <span class="inline-flex items-center gap-1.5 text-xs" :class="LED_STYLE[row.led ?? 'Normal']">
          <span class="h-2 w-2 rounded-full bg-current" />
          {{ row.led ?? 'Normal' }}
        </span>
      </template>
      <template #cell-rma="{ row }">
        <span v-if="row.rma_date" class="text-xs text-rose-400">{{ date(row.rma_date) }}</span>
        <span v-else class="text-xs text-slate-600">—</span>
      </template>
      <template #cell-actions="{ row }">
        <div class="flex justify-end gap-1.5">
          <button
            type="button"
            class="rounded-md border border-surface-600 px-2 py-1 text-[11px] font-medium text-slate-300 transition-colors hover:border-sky-500 hover:text-sky-400"
            title="Toggle locate LED"
          >
            Locate
          </button>
          <button
            type="button"
            class="rounded-md border border-surface-600 px-2 py-1 text-[11px] font-medium text-slate-300 transition-colors hover:border-rose-500 hover:text-rose-400"
            title="Mark for RMA"
          >
            RMA
          </button>
        </div>
      </template>
    </ResourceTable>

    <p class="mt-3 text-xs text-slate-500">
      First-seen and RMA dates are tracked per serial number. LED control wraps <code class="font-mono">ledctl</code> on the host.
    </p>
  </div>
</template>
