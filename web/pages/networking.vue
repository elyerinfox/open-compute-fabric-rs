<script setup lang="ts">
const api = useApi()

const { data } = await useAsyncData('networking', async () => {
  const [vpcs, subnets] = await Promise.all([api.listVpcs(), api.listSubnets()])
  return { vpcs, subnets }
})

const vpcs = computed(() => data.value?.vpcs.data ?? [])
const subnets = computed(() => data.value?.subnets.data ?? [])
const source = computed<'live' | 'mock'>(() =>
  data.value && data.value.vpcs.source === 'live' && data.value.subnets.source === 'live'
    ? 'live'
    : 'mock',
)

// Which VPC's subnets are shown; null = all.
const selectedVpc = ref<string | null>(null)

const visibleSubnets = computed(() =>
  selectedVpc.value ? subnets.value.filter((s) => s.vpc_id === selectedVpc.value) : subnets.value,
)

function subnetCountFor(vpcId: string): number {
  return subnets.value.filter((s) => s.vpc_id === vpcId).length
}
</script>

<template>
  <div>
    <PageHeader
      title="Networking"
      subtitle="VPC isolation domains and their subnets (network namespaces)."
      :source="source"
    />

    <!-- VPCs -->
    <h2 class="mb-3 text-sm font-semibold text-slate-300">VPCs</h2>
    <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
      <button
        v-for="vpc in vpcs"
        :key="vpc.metadata.id"
        type="button"
        class="ocf-card p-5 text-left transition-colors hover:border-brand-600/50"
        :class="selectedVpc === vpc.metadata.id ? 'border-brand-600/60 ring-1 ring-inset ring-brand-600/30' : ''"
        @click="selectedVpc = selectedVpc === vpc.metadata.id ? null : vpc.metadata.id"
      >
        <div class="flex items-start justify-between">
          <div>
            <p class="font-semibold text-slate-100">{{ vpc.metadata.name }}</p>
            <p class="mt-1 font-mono text-xs text-slate-400">{{ vpc.cidr }}</p>
          </div>
          <span class="rounded-md bg-surface-800 px-2 py-1 font-mono text-xs text-slate-400">
            VNI {{ vpc.vni }}
          </span>
        </div>
        <div class="mt-4 flex items-center gap-4 text-xs text-slate-500">
          <span>{{ subnetCountFor(vpc.metadata.id) }} subnets</span>
          <span v-if="vpc.metadata.labels?.env" class="rounded bg-surface-800 px-1.5 py-0.5 font-mono">
            env={{ vpc.metadata.labels.env }}
          </span>
        </div>
      </button>
    </div>

    <!-- Subnets -->
    <div class="mt-8 mb-3 flex items-center justify-between">
      <h2 class="text-sm font-semibold text-slate-300">
        Subnets
        <span v-if="selectedVpc" class="ml-1 font-normal text-slate-500">
          · filtered by {{ vpcs.find((v) => v.metadata.id === selectedVpc)?.metadata.name }}
        </span>
      </h2>
      <button
        v-if="selectedVpc"
        type="button"
        class="text-xs font-medium text-brand-400 hover:text-brand-300"
        @click="selectedVpc = null"
      >
        Clear filter
      </button>
    </div>

    <ResourceTable
      :columns="[
        { key: 'name', label: 'Name' },
        { key: 'vpc', label: 'VPC' },
        { key: 'cidr', label: 'CIDR', mono: true },
        { key: 'netns', label: 'Namespace', mono: true },
      ]"
      :rows="visibleSubnets"
      :row-key="(s) => s.metadata.id"
      empty="No subnets in this VPC."
    >
      <template #cell-name="{ row }">
        <span class="font-medium text-slate-200">{{ row.metadata.name }}</span>
      </template>
      <template #cell-vpc="{ row }">
        <span class="rounded bg-surface-800 px-2 py-0.5 text-xs text-slate-300">{{ row.vpc_name ?? row.vpc_id }}</span>
      </template>
    </ResourceTable>
  </div>
</template>
