<script setup lang="ts">
import type { TopologyNode } from '~/composables/types'

const api = useApi()
const { bytes, number } = useFormat()

const { data } = await useAsyncData('topology', () => api.getTopologyTree())
const tree = computed(() => data.value?.data)
const source = computed<'live' | 'mock'>(() => data.value?.source ?? 'mock')

// Selected node drives the detail panel. Default to the first machine found.
const selected = ref<TopologyNode | null>(null)

function firstMachine(node: TopologyNode | undefined): TopologyNode | null {
  if (!node) return null
  if (node.level === 'Machine') return node
  for (const c of node.children ?? []) {
    const m = firstMachine(c)
    if (m) return m
  }
  return null
}

watchEffect(() => {
  if (!selected.value && tree.value) {
    selected.value = firstMachine(tree.value) ?? tree.value
  }
})

function onSelect(node: TopologyNode) {
  selected.value = node
}

const machine = computed(() => selected.value?.machine ?? null)
</script>

<template>
  <div>
    <PageHeader
      title="Topology"
      subtitle="Drill down: region → datacenter → rack → machine. Click a node for details."
      :source="source"
    />

    <div class="grid grid-cols-1 gap-4 lg:grid-cols-5">
      <!-- Tree -->
      <div class="ocf-card p-3 lg:col-span-3">
        <div class="px-2 py-2">
          <h2 class="text-sm font-semibold text-slate-300">Fleet Hierarchy</h2>
        </div>
        <div class="mt-1 space-y-0.5">
          <TreeNode
            v-if="tree"
            :node="tree"
            :selected-id="selected?.id"
            @select="onSelect"
          />
        </div>
      </div>

      <!-- Detail panel -->
      <div class="lg:col-span-2">
        <div class="ocf-card sticky top-6 p-5">
          <template v-if="selected">
            <div class="flex items-start justify-between gap-3">
              <div>
                <p class="text-[11px] font-semibold uppercase tracking-wide text-slate-500">
                  {{ selected.level }}
                </p>
                <h2 class="mt-0.5 text-lg font-semibold text-slate-50">{{ selected.name }}</h2>
              </div>
              <HealthBadge :status="selected.health" />
            </div>

            <!-- Machine-specific detail -->
            <template v-if="machine">
              <dl class="mt-5 space-y-3 text-sm">
                <div class="flex justify-between">
                  <dt class="text-slate-400">Hostname</dt>
                  <dd class="font-mono text-xs text-slate-300">{{ machine.hostname }}</dd>
                </div>
                <div class="flex justify-between">
                  <dt class="text-slate-400">State</dt>
                  <dd><HealthBadge :status="machine.state" /></dd>
                </div>
                <div class="flex justify-between">
                  <dt class="text-slate-400">IP address</dt>
                  <dd class="font-mono text-xs text-slate-300">{{ machine.ip_address ?? '—' }}</dd>
                </div>
                <div class="flex justify-between">
                  <dt class="text-slate-400">Rack</dt>
                  <dd class="text-slate-300">{{ machine.rack ?? '—' }}</dd>
                </div>
              </dl>

              <div class="mt-5 grid grid-cols-3 gap-3">
                <div class="rounded-lg bg-surface-850/60 p-3 text-center">
                  <p class="text-xs text-slate-500">vCPU</p>
                  <p class="mt-1 text-base font-semibold text-slate-100">{{ machine.cpu_cores }}</p>
                </div>
                <div class="rounded-lg bg-surface-850/60 p-3 text-center">
                  <p class="text-xs text-slate-500">Memory</p>
                  <p class="mt-1 text-base font-semibold text-slate-100">{{ bytes(machine.memory_bytes) }}</p>
                </div>
                <div class="rounded-lg bg-surface-850/60 p-3 text-center">
                  <p class="text-xs text-slate-500">Disk</p>
                  <p class="mt-1 text-base font-semibold text-slate-100">{{ bytes(machine.disk_bytes) }}</p>
                </div>
              </div>

              <div class="mt-5 flex items-center justify-between rounded-lg border border-surface-700 px-4 py-3">
                <span class="text-sm text-slate-400">Workloads on this node</span>
                <span class="text-sm font-semibold text-slate-100">{{ number(machine.workload_count) }}</span>
              </div>

              <div v-if="machine.labels && Object.keys(machine.labels).length" class="mt-5">
                <p class="text-xs font-medium text-slate-500">Labels</p>
                <div class="mt-2 flex flex-wrap gap-1.5">
                  <span
                    v-for="(v, k) in machine.labels"
                    :key="k"
                    class="rounded-md bg-surface-800 px-2 py-0.5 font-mono text-xs text-slate-300"
                  >
                    {{ k }}={{ v }}
                  </span>
                </div>
              </div>

              <div class="mt-6 flex gap-2">
                <button
                  type="button"
                  class="flex-1 rounded-lg bg-brand-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-brand-500"
                >
                  Cordon
                </button>
                <button
                  type="button"
                  class="flex-1 rounded-lg border border-surface-600 px-3 py-2 text-sm font-medium text-slate-300 transition-colors hover:bg-surface-800"
                >
                  Power
                </button>
              </div>
            </template>

            <!-- Non-machine summary -->
            <template v-else>
              <div class="mt-5 rounded-lg border border-surface-700 px-4 py-3 text-sm text-slate-400">
                <p>
                  This {{ (selected.level ?? 'node').toLowerCase() }} contains
                  <span class="font-semibold text-slate-200">{{ selected.children?.length ?? 0 }}</span>
                  child {{ (selected.children?.length ?? 0) === 1 ? 'node' : 'nodes' }}.
                </p>
                <p class="mt-2 text-xs text-slate-500">
                  Expand it in the tree and select a machine to see hardware detail.
                </p>
              </div>
            </template>

            <p class="mt-5 font-mono text-[11px] text-slate-600">id: {{ selected.id }}</p>
          </template>

          <p v-else class="text-sm text-slate-500">Select a node from the tree.</p>
        </div>
      </div>
    </div>
  </div>
</template>
