<script setup lang="ts">
import type { TopologyNode } from '~/composables/types'

const api = useApi()
const { bytes, number, bitsPerSec } = useFormat()

// Pull the data the dashboard summarizes. useAsyncData caches per-key and runs
// on the server for the first paint.
const { data } = await useAsyncData('dashboard', async () => {
  const [topology, workloads, disks, host, lbs, health] = await Promise.all([
    api.getTopologyTree(),
    api.listWorkloads(),
    api.listDisks(),
    api.getHostMetrics(),
    api.listLoadBalancers(),
    api.getHealthFindings(),
  ])
  return { topology, workloads, disks, host, lbs, health }
})

// All calls share the same source; if any fell back, surface "mock".
const source = computed<'live' | 'mock'>(() => {
  const d = data.value
  if (!d) return 'mock'
  const all = [d.topology, d.workloads, d.disks, d.host, d.lbs, d.health]
  return all.every((r) => r.source === 'live') ? 'live' : 'mock'
})

// Fleet-health findings needing attention (warnings + criticals).
const healthFindings = computed(() => data.value?.health.data ?? [])
const healthAttention = computed(
  () => healthFindings.value.filter((f) => f.severity === 'warning' || f.severity === 'critical').length,
)

// Flatten the topology tree to count machines and their health.
function flattenMachines(node: TopologyNode | undefined, acc: TopologyNode[] = []): TopologyNode[] {
  if (!node) return acc
  if (node.level === 'Machine') acc.push(node)
  for (const c of node.children ?? []) flattenMachines(c, acc)
  return acc
}

const machines = computed(() => flattenMachines(data.value?.topology.data))
const machineCount = computed(() => machines.value.length)
const healthyMachines = computed(() => machines.value.filter((m) => m.health === 'Healthy').length)

const workloads = computed(() => data.value?.workloads.data ?? [])
const runningWorkloads = computed(() => workloads.value.filter((w) => w.state === 'Running').length)
const haWorkloads = computed(() => workloads.value.filter((w) => w.highly_available).length)

const disks = computed(() => data.value?.disks.data ?? [])
const failingDisks = computed(
  () => disks.value.filter((d) => d.health === 'Failing' || d.health === 'Warning').length,
)

const host = computed(() => data.value?.host.data)
const lbCount = computed(() => data.value?.lbs.data.length ?? 0)

// Fleet-wide health rollup.
const fleetHealth = computed(() => {
  if (machineCount.value === 0) return 'Unknown'
  if (healthyMachines.value === machineCount.value) return 'Healthy'
  if (healthyMachines.value === 0) return 'Unhealthy'
  return 'Degraded'
})

const memPct = computed(() => {
  const h = host.value
  if (!h || !h.memory_total) return 0
  return Math.round((h.memory_used / h.memory_total) * 100)
})
const diskPct = computed(() => {
  const h = host.value
  if (!h || !h.disk_total) return 0
  return Math.round((h.disk_used / h.disk_total) * 100)
})
</script>

<template>
  <div>
    <PageHeader
      title="Dashboard"
      subtitle="Fleet-wide summary of compute, workloads, and health."
      :source="source"
    />

    <!-- Summary cards -->
    <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <NuxtLink to="/health" class="block transition-transform hover:-translate-y-0.5">
        <StatCard
          label="Health"
          :value="healthAttention"
          :hint="healthAttention > 0 ? 'warnings need attention' : 'all checks passing'"
          :accent="healthAttention > 0 ? 'amber' : 'emerald'"
        >
          <template #icon>
            <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
              <path stroke-linecap="round" stroke-linejoin="round" d="M3 12h4l2 5 4-12 2 7h6" />
            </svg>
          </template>
        </StatCard>
      </NuxtLink>

      <StatCard
        label="Machines"
        :value="machineCount"
        :hint="`${healthyMachines} healthy`"
        accent="brand"
      >
        <template #icon>
          <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path stroke-linecap="round" stroke-linejoin="round" d="M4 5h16v10H4V5zm-1 14h18" />
          </svg>
        </template>
      </StatCard>

      <StatCard
        label="Workloads"
        :value="workloads.length"
        :hint="`${runningWorkloads} running · ${haWorkloads} HA`"
        accent="violet"
      >
        <template #icon>
          <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path stroke-linecap="round" stroke-linejoin="round" d="M4 6h16M4 12h16M4 18h16" />
          </svg>
        </template>
      </StatCard>

      <StatCard
        label="Load Balancers"
        :value="lbCount"
        hint="TCP + application"
        accent="sky"
      >
        <template #icon>
          <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path stroke-linecap="round" stroke-linejoin="round" d="M12 3v6m-4 4l4-4 4 4M5 13h14v6H5z" />
          </svg>
        </template>
      </StatCard>

      <StatCard
        label="Disks at Risk"
        :value="failingDisks"
        :hint="`${disks.length} total disks`"
        :accent="failingDisks > 0 ? 'amber' : 'emerald'"
      >
        <template #icon>
          <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path stroke-linecap="round" stroke-linejoin="round" d="M4 7c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3z" />
          </svg>
        </template>
      </StatCard>
    </div>

    <!-- Health + host utilization -->
    <div class="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-3">
      <!-- Fleet health -->
      <div class="ocf-card p-5 lg:col-span-1">
        <h2 class="text-sm font-semibold text-slate-300">Fleet Health</h2>
        <div class="mt-4 flex items-center gap-3">
          <HealthBadge :status="fleetHealth" />
          <span class="text-sm text-slate-400">
            {{ healthyMachines }}/{{ machineCount }} machines healthy
          </span>
        </div>
        <ul class="mt-5 space-y-3 text-sm">
          <li class="flex items-center justify-between">
            <span class="text-slate-400">Running workloads</span>
            <span class="font-medium text-slate-200">{{ runningWorkloads }}</span>
          </li>
          <li class="flex items-center justify-between">
            <span class="text-slate-400">Highly available</span>
            <span class="font-medium text-slate-200">{{ haWorkloads }}</span>
          </li>
          <li class="flex items-center justify-between">
            <span class="text-slate-400">Disks needing attention</span>
            <span class="font-medium" :class="failingDisks > 0 ? 'text-amber-400' : 'text-slate-200'">
              {{ failingDisks }}
            </span>
          </li>
        </ul>
      </div>

      <!-- Host utilization -->
      <div class="ocf-card p-5 lg:col-span-2">
        <h2 class="text-sm font-semibold text-slate-300">Host Utilization (aggregate)</h2>
        <div class="mt-5 grid grid-cols-1 gap-5 sm:grid-cols-3">
          <div>
            <div class="flex items-baseline justify-between">
              <span class="text-sm text-slate-400">CPU</span>
              <span class="text-sm font-medium text-slate-200">{{ host?.cpu_pct?.toFixed(1) ?? '—' }}%</span>
            </div>
            <div class="mt-2 h-2 overflow-hidden rounded-full bg-surface-700">
              <div class="h-full rounded-full bg-brand-500" :style="{ width: `${host?.cpu_pct ?? 0}%` }" />
            </div>
          </div>
          <div>
            <div class="flex items-baseline justify-between">
              <span class="text-sm text-slate-400">Memory</span>
              <span class="text-sm font-medium text-slate-200">{{ memPct }}%</span>
            </div>
            <div class="mt-2 h-2 overflow-hidden rounded-full bg-surface-700">
              <div class="h-full rounded-full bg-violet-500" :style="{ width: `${memPct}%` }" />
            </div>
            <p class="mt-1 text-xs text-slate-500">{{ bytes(host?.memory_used) }} / {{ bytes(host?.memory_total) }}</p>
          </div>
          <div>
            <div class="flex items-baseline justify-between">
              <span class="text-sm text-slate-400">Disk</span>
              <span class="text-sm font-medium text-slate-200">{{ diskPct }}%</span>
            </div>
            <div class="mt-2 h-2 overflow-hidden rounded-full bg-surface-700">
              <div class="h-full rounded-full bg-sky-500" :style="{ width: `${diskPct}%` }" />
            </div>
            <p class="mt-1 text-xs text-slate-500">{{ bytes(host?.disk_used) }} / {{ bytes(host?.disk_total) }}</p>
          </div>
        </div>

        <div class="mt-6 grid grid-cols-2 gap-4 sm:grid-cols-4">
          <div class="rounded-lg bg-surface-850/60 p-3">
            <p class="text-xs text-slate-500">Net RX</p>
            <p class="mt-1 text-sm font-medium text-slate-200">{{ bitsPerSec(host?.net_rx_bps) }}</p>
          </div>
          <div class="rounded-lg bg-surface-850/60 p-3">
            <p class="text-xs text-slate-500">Net TX</p>
            <p class="mt-1 text-sm font-medium text-slate-200">{{ bitsPerSec(host?.net_tx_bps) }}</p>
          </div>
          <div class="rounded-lg bg-surface-850/60 p-3">
            <p class="text-xs text-slate-500">Read IOPS</p>
            <p class="mt-1 text-sm font-medium text-slate-200">{{ number(host?.read_iops) }}</p>
          </div>
          <div class="rounded-lg bg-surface-850/60 p-3">
            <p class="text-xs text-slate-500">Write IOPS</p>
            <p class="mt-1 text-sm font-medium text-slate-200">{{ number(host?.write_iops) }}</p>
          </div>
        </div>
      </div>
    </div>

    <!-- Recent workloads preview -->
    <div class="mt-6">
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-sm font-semibold text-slate-300">Workloads</h2>
        <NuxtLink to="/workloads" class="text-xs font-medium text-brand-400 hover:text-brand-300">
          View all →
        </NuxtLink>
      </div>
      <ResourceTable
        :columns="[
          { key: 'name', label: 'Name' },
          { key: 'kind', label: 'Kind' },
          { key: 'state', label: 'State' },
          { key: 'node', label: 'Node' },
        ]"
        :rows="workloads.slice(0, 5)"
        :row-key="(w) => w.metadata.id"
      >
        <template #cell-name="{ row }">
          <span class="font-medium text-slate-200">{{ row.metadata.name }}</span>
        </template>
        <template #cell-state="{ row }">
          <HealthBadge :status="row.state" />
        </template>
        <template #cell-node="{ row }">
          <span class="font-mono text-xs text-slate-400">{{ row.node_name ?? '—' }}</span>
        </template>
      </ResourceTable>
    </div>
  </div>
</template>
