<script setup lang="ts">
import type { Workload } from '~/composables/types'

const api = useApi()
const { bytes, millicores } = useFormat()

const { data, refresh } = await useAsyncData('workloads', () => api.listWorkloads())
const workloads = computed(() => data.value?.data ?? [])
const source = computed<'live' | 'mock'>(() => data.value?.source ?? 'mock')

// Filter controls.
const search = ref('')
const kindFilter = ref<'All' | 'Container' | 'VirtualMachine'>('All')

const filtered = computed(() =>
  workloads.value.filter((w) => {
    const matchesKind = kindFilter.value === 'All' || w.kind === kindFilter.value
    const q = search.value.trim().toLowerCase()
    const matchesSearch =
      !q ||
      w.metadata.name.toLowerCase().includes(q) ||
      w.image.toLowerCase().includes(q) ||
      (w.node_name ?? '').toLowerCase().includes(q)
    return matchesKind && matchesSearch
  }),
)

// Migrate action stub. Opens a confirmation, calls the (stubbed) endpoint.
const migrating = ref<string | null>(null)
const toast = ref<string | null>(null)

async function migrate(w: Workload) {
  if (!w.highly_available && w.kind === 'VirtualMachine') {
    // Allowed but flag a note; QEMU supports migration in the contract.
  }
  migrating.value = w.metadata.id
  try {
    const res = await api.migrateWorkload(w.metadata.id)
    toast.value =
      res.source === 'mock'
        ? `Migration of "${w.metadata.name}" queued (mock).`
        : `Migration of "${w.metadata.name}" accepted.`
    await refresh()
  } finally {
    migrating.value = null
    setTimeout(() => (toast.value = null), 4000)
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Workloads"
      subtitle="Containers and virtual machines running across the fleet."
      :source="source"
    />

    <!-- Controls -->
    <div class="mb-4 flex flex-wrap items-center gap-3">
      <div class="relative flex-1 sm:max-w-xs">
        <svg
          class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-500"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path stroke-linecap="round" stroke-linejoin="round" d="M21 21l-4.3-4.3M11 19a8 8 0 100-16 8 8 0 000 16z" />
        </svg>
        <input
          v-model="search"
          type="search"
          placeholder="Search name, image, node…"
          class="w-full rounded-lg border border-surface-700 bg-surface-900 py-2 pl-9 pr-3 text-sm text-slate-200 placeholder:text-slate-500 focus:border-brand-500 focus:outline-none focus:ring-1 focus:ring-brand-500"
        />
      </div>

      <div class="inline-flex rounded-lg border border-surface-700 bg-surface-900 p-0.5">
        <button
          v-for="opt in ['All', 'Container', 'VirtualMachine']"
          :key="opt"
          type="button"
          class="rounded-md px-3 py-1.5 text-xs font-medium transition-colors"
          :class="
            kindFilter === opt
              ? 'bg-brand-600 text-white'
              : 'text-slate-400 hover:text-slate-200'
          "
          @click="kindFilter = opt as any"
        >
          {{ opt === 'VirtualMachine' ? 'VM' : opt }}
        </button>
      </div>
    </div>

    <!-- Toast -->
    <div
      v-if="toast"
      class="mb-4 rounded-lg border border-brand-600/30 bg-brand-600/10 px-4 py-2.5 text-sm text-brand-300"
    >
      {{ toast }}
    </div>

    <ResourceTable
      :columns="[
        { key: 'name', label: 'Name' },
        { key: 'kind', label: 'Kind' },
        { key: 'image', label: 'Image' },
        { key: 'resources', label: 'Resources' },
        { key: 'state', label: 'State' },
        { key: 'node', label: 'Node' },
        { key: 'actions', label: '', align: 'right' },
      ]"
      :rows="filtered"
      :row-key="(w) => w.metadata.id"
      empty="No workloads match your filters."
    >
      <template #cell-name="{ row }">
        <div class="flex items-center gap-2">
          <span class="font-medium text-slate-200">{{ row.metadata.name }}</span>
          <span
            v-if="row.highly_available"
            class="rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-emerald-400"
            title="Highly available"
          >
            HA
          </span>
        </div>
      </template>

      <template #cell-kind="{ row }">
        <span class="inline-flex items-center gap-1.5 text-slate-300">
          <svg
            class="h-4 w-4 text-slate-500"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
          >
            <path
              v-if="row.kind === 'Container'"
              stroke-linecap="round"
              stroke-linejoin="round"
              d="M4 7h16v10H4V7zm5 0v10m6-10v10"
            />
            <path
              v-else
              stroke-linecap="round"
              stroke-linejoin="round"
              d="M4 5h16v10H4V5zm-1 14h18M9 19v-4m6 4v-4"
            />
          </svg>
          {{ row.kind === 'VirtualMachine' ? 'VM' : 'Container' }}
        </span>
      </template>

      <template #cell-image="{ row }">
        <span class="font-mono text-xs text-slate-400">{{ row.image }}</span>
      </template>

      <template #cell-resources="{ row }">
        <span class="font-mono text-xs text-slate-400">
          {{ millicores(row.resources.cpu_millis) }} · {{ bytes(row.resources.memory_bytes) }}
        </span>
      </template>

      <template #cell-state="{ row }">
        <HealthBadge :status="row.state" />
      </template>

      <template #cell-node="{ row }">
        <span class="font-mono text-xs text-slate-400">{{ row.node_name ?? '— unscheduled' }}</span>
      </template>

      <template #cell-actions="{ row }">
        <button
          type="button"
          class="rounded-lg border border-surface-600 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:border-brand-500 hover:text-brand-400 disabled:cursor-not-allowed disabled:opacity-50"
          :disabled="migrating === row.metadata.id || row.state !== 'Running'"
          :title="row.state !== 'Running' ? 'Only running workloads can be migrated' : 'Migrate to another node'"
          @click="migrate(row)"
        >
          <span v-if="migrating === row.metadata.id">Migrating…</span>
          <span v-else>Migrate</span>
        </button>
      </template>
    </ResourceTable>

    <p class="mt-3 text-xs text-slate-500">
      Migration orchestrates dump → transfer → restore between runtime providers, honoring each
      workload's placement scope.
    </p>
  </div>
</template>
